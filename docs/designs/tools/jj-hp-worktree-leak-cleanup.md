# Design: jj-hp hook worktrees — leak-proof cleanup

Status: **proposed**
Domain: tools

Tracking issue: RIG-4294. Crate: `jj-hooks` (this repo, the `jj-hp` binary),
currently `0.3.12`.

## Problem / Intent

On mattfw (2026-10-04) the 63G `/tmp` tmpfs filled and jj-hp hooks failed with
ENOSPC. Eight `/tmp/jj-hooks-worktree-*` dirs held 1.3–1.9G each, mostly
`node_modules` unable to hardlink from the bun cache on `/home`. Two had leaked:
no process used them, but git admin entries remained. Another 11G was
`/tmp/node-compile-cache`.

`run_once` in `src/hooks.rs` creates the worktree, then blocks on hook children:

```rust
let wt = Worktree::create(primary_git_dir, target_commit)?;
```

Cleanup is one Drop impl in `src/worktree.rs`:

```rust
impl Drop for Worktree {
    fn drop(&mut self) {
        if let Err(e) = self.remove() {
```

Drop runs on return, `?` and panic unwind. It does not run when a signal kills
the process by default action, and nothing reaps the leak later. Also,
`TempDir::with_prefix("jj-hooks-worktree-")?` resolves through
`std::env::temp_dir()`, so every worktree lands in RAM.

Intent:

- jj-hp removes its hook worktree on every exit path it can observe.
- The next jj-hp run that creates a hook worktree reaps worktrees whose owner
  died unobserved (SIGKILL, OOM, power loss).
- Worktrees move to an on-disk root.

Out of scope (nix-config follow-up): `NODE_COMPILE_CACHE`, `GOCACHE` and
`TMPDIR` on disk, and a systemd-tmpfiles age rule for legacy
`/tmp/jj-hooks-worktree-*` dirs.

## Global Constraints

- MSRV 1.89: std `File::lock`/`try_lock` and `CommandExt::process_group`.
- No `unsafe`. Signals go through `signal-hook` (an iterator thread); kill and
  wait go through `rustix` (feature `process`), both under
  `[target.'cfg(unix)'.dependencies]`. Non-unix keeps plain `Command`.
- Only `run()` (both binaries) installs signal handling; library callers keep
  default behaviour.
- With no live worktree, a signal takes its default action, as today. On Linux
  a signal ignored at startup stays ignored.
- Cleanup and sweep are best-effort: `tracing::warn!` and go on.
- An interrupt is an error, never a cancellation: `Cancel` short-circuits "with
  `success: true`" (its doc in `src/hooks.rs`).
- jj-hp never runs `git worktree prune`. It deletes a checkout or admin entry
  only on a dead-owner lock that it wrote.
- Keep `panic = "unwind"`: Drop on unwind is part of the contract.
- Nothing is written inside the checkout (`maybe_build_fixup_commit` runs
  `git add -A` there).
- Code comments are 1–2 lines.

## Approach

Three layers: Drop (exists) for return, `?` and panic; a signal watcher (new)
that turns signals into `Interrupted` so Drop runs; a sweep (new) for SIGKILL,
OOM and crashes.

Layout under a configurable on-disk root:

```text
<root>/jj-hooks-worktree-XXXXXX/      # the checkout (TempDir::with_prefix_in)
<root>/jj-hooks-worktree-XXXXXX.lock  # flock held for the Worktree's life; "pid\ngit_dir\n"
```

The checkout keeps the prefix as its basename, so git's admin entry does too.

New abstraction: `src/interrupt.rs`, process-global state that a watcher thread
writes and spawn sites read. The per-workspace caches have no writer thread.

### Fork 1 — cleanup on signal or kill

**Chosen:** keep the RAII Drop guard and add a `signal-hook` watcher thread.
The Fork 2 sweep is the SIGKILL backstop.

Watched: SIGINT, SIGTERM and SIGHUP, minus any set in the `SigIgn` line of
`/proc/self/status` at install, so `nohup` keeps working. Other unix targets
cannot read the disposition without `unsafe`, so they watch only SIGINT and
SIGTERM.

One mutex, `STATE`, holds the live count, the first signal, the hit count and
the child registry; the watcher and `LiveWorktree` act only while holding it.
Per signal:

- `live == 0`: `emulate_default_handler(sig)`, as today.
- Otherwise: record the first signal and bump `hits`. Send each registered
  child (its group when `group`) SIGTERM on hit 1 and SIGKILL on hit 2. Hit 3
  calls `emulate_default_handler(sig)`.

It forwards SIGTERM because many runners read a second SIGINT as "force quit".

Spawn helpers keep this order (the PID-reuse invariant):

1. Under `STATE`: if `hits > 0`, return `Interrupted`. Else spawn (with pipe
   readers if captured) and register `{pid, group}`, so the child sees every
   later escalation.
2. `waitid(Pid, EXITED | NOWAIT)`: wait for exit without reaping.
3. If captured: join the readers, but stop once interrupted, so a grandchild
   holding the pipe cannot block the abort.
4. If interrupted and `group`: `kill_process_group(pid, KILL)`. The unreaped
   leader pins the PGID.
5. Under `STATE`: deregister. Then `child.wait()` reaps.
6. `check()?`.

So a registered PID is always a live child or our zombie.

Groups: at install, jj-hp tries `File::open("/dev/tty")`. Without a controlling
terminal (fleet agents), every spawn gets `process_group(0)`, so group signals
reach grandchildren. With one, children stay in jj-hp's group: a background
group would stop on SIGTTIN at a `/dev/tty` prompt (SSH, credentials), and
Ctrl-C already reaches every descendant. The sweep catches stragglers there.

Why not `PR_SET_PDEATHSIG`: it never removes the dir, is Linux-only, needs an
`unsafe` `pre_exec`, and fires when the spawning `thread::scope` worker exits.

Why not sweep alone: every Ctrl-C would leave 1.8G until the next run.

### Fork 2 — leak sweep

**Chosen:** an owner lock, not a PID. `create` makes the checkout dir and
`<name>.lock`, takes `File::lock()` and writes `pid\ngit_dir\n` before
`git worktree add`. The kernel drops a `flock` on any death, so a successful
`try_lock()` means a dead owner. The lock is also the ownership proof.

`sweep(root, grace)` runs once per process per root, at the first
`Worktree::create`. For each `jj-hooks-worktree-*` entry:

- Lock `WouldBlock`: the owner is live; skip.
- Lock taken and the content parses: run
  `git --git-dir=<g> worktree remove --force --force <dir>` (failure ignored),
  remove the dir if present, and unlink the lock. This also clears an orphan
  lock's admin entry.
- No lock, or an empty one: a creator may be mid-create, so skip while younger
  than `grace` (1h, by mtime). Then `remove_dir` (empty dirs only) and unlink
  the empty lock. Content follows the lock write, so a populated jj-hp checkout
  always has a parseable lock.
- A malformed lock: warn and skip.

`--force --force`: `add` locks its entry as "initializing", and git-worktree(1)
says "To remove a locked worktree, specify --force twice." Measured on git
2.55: it drops the entry of a missing path, also through a symlinked parent, so
roots need no canonicalizing.

Why not `git worktree prune`: it touches every worktree, including a user's on
an unmounted disk.

Why not age-based removal: it deletes slow live gates or keeps leaks for hours.

Why not a PID marker: PID reuse can make the verdict wrong in both directions.

### Fork 3 — worktree root

**Chosen:** an on-disk XDG cache root that the user can configure, with
precedence like `gate_cache_enabled`:

1. env `JJ_HOOKS_WORKTREE_ROOT` (absolute);
2. jj config `jj-hooks.worktree-root`;
3. `$XDG_CACHE_HOME/jj-hooks/worktrees` (if absolute);
4. `std::env::home_dir()` (which falls back to `getpwuid_r` without `HOME`)
   joined with `.cache/jj-hooks/worktrees`;
5. `std::env::temp_dir()/jj-hooks-worktrees` with a tmpfs warning, only when no
   home dir is known.

A relative override warns and falls through.

`~/.cache` shares `/home` with the bun cache, so bun's hardlinks
([bun install docs](https://bun.com/docs/pm/cli/install)) cost no RAM.

Why not `std::env::temp_dir()`: it is tmpfs unless `TMPDIR` is set, a host-wide
choice for the nix follow-up.

## Plan

Land T1 to T4 in order; release after T4. Each task's test cycle is
`cargo fmt -p jj-hooks -- --check`,
`cargo clippy -p jj-hooks --all-targets -- -D warnings` and
`cargo nextest run -p jj-hooks`. Each new test is red before its task.

### T1 — on-disk root and owner lock

`src/worktree.rs`: the root resolver, the sibling owner lock, and a
`--force --force` remove (used by Drop and on add failure). `src/hooks.rs`:
resolve the root once at `run_for_update`, `run_for_updates_parallel` and
`run_for_partitioned_updates_parallel`, and pass it down to `run_once`.

```rust
// src/worktree.rs
pub const WORKTREE_PREFIX: &str = "jj-hooks-worktree-";
pub fn worktree_root(jj: &crate::jj::JjCli) -> PathBuf;
// Pure precedence core; unit-tested.
fn root_from(env: Option<&OsStr>, config: Option<&str>, xdg_cache: Option<&OsStr>, home: Option<&Path>, tmp: &Path) -> PathBuf;
// Drop order: `checkout` before `owner`, so the lock outlives the dir.
pub struct Worktree { git_dir: PathBuf, checkout: TempDir, lock_path: PathBuf, owner: File, removed: bool }
impl Worktree {
    pub fn create(root: &Path, git_dir: &Path, commit: &str) -> Result<Self>;
    pub fn path(&self) -> &Path;
    pub fn git_dir(&self) -> &Path;
}
// remove(): worktree remove --force --force, remove_dir_all, unlink; stops at the first failure.
// src/hooks.rs (private): run_for_update_with_cancel and run_once gain `root: &Path`.
```

`tests/workspace.rs` passes a root inside the test's tmp dir.

New `tests/worktree_cleanup.rs`: fixture `PRE_PUSH_RECORD_CWD` runs
`sh -c 'printf "%s" "$PWD" > "$JJ_HOOKS_TEST_CWD_OUT"'`, plus a failing twin
(`exit 1`). For each, push with `JJ_HOOKS_WORKTREE_ROOT=<tmp>/root` and assert:

- the recorded cwd is under `<tmp>/root/` and no longer exists;
- `<tmp>/root` is empty;
- `git worktree list --porcelain` lists only the primary.

Red today: the cwd is under `/tmp`. Units cover `root_from` precedence.

### T2 — sweep

```rust
const CREATION_GRACE: Duration = Duration::from_secs(3600);
// Reaps dead-owner checkouts under root; returns the reap count.
pub(crate) fn sweep(root: &Path, grace: Duration) -> usize;
```

A once-per-process gate (`static SWEPT: Mutex<HashSet<PathBuf>>`) calls it with
`CREATION_GRACE` at the top of `Worktree::create`.

Harness: `jj_hooks_spawn_with_env(&self, args, extra_env, log: &Path) -> Child`
(stdin null, output to `log`). Fixture `PRE_PUSH_SLEEPER` records `$PWD`, starts
`(while :; do date > tick; sleep 0.1; done) &` (it holds stdout), records `$!`
to `$JJ_HOOKS_TEST_PID_OUT`, and runs `wait`.

Tests:

- SIGKILL jj-hp and the loop once the cwd file exists, then push again with the
  same root: the root is empty and only the primary is listed. Red after T1.
- `sweep` units (zero grace unless stated):
  - a held lock survives;
  - an unlocked parseable lock is reaped with its admin entry, also under a
    symlinked root;
  - an orphan parseable lock drops its missing path's admin entry;
  - a lock-less user worktree `<root>/jj-hooks-worktree-user` survives and
    stays listed;
  - an empty dir with an empty lock is removed, but survives `CREATION_GRACE`.

### T3 — signal watcher

```rust
// src/error.rs
#[error("interrupted by signal {signal}")]
Interrupted { signal: i32 },

// src/interrupt.rs: signal parts are #[cfg(unix)]; elsewhere install/check are no-ops.
static STATE: Mutex<State> = Mutex::new(State::new());
struct State { live: usize, signal: i32, hits: u32, children: Vec<Tracked> } // signal 0 = none
struct Tracked { pid: rustix::process::Pid, group: bool }
enum Action { Default, Send(rustix::process::Signal) }
// Pure watcher step: live == 0 → Default, records nothing; else hit 1 TERM, 2 KILL, 3+ Default.
fn divert(state: &mut State, sig: i32) -> Action;
// Idempotent. The flag is sticky: hosts that outlive an interrupt must not call it.
pub fn install();
pub fn signal() -> Option<i32>;
pub fn check() -> Result<()>; // Err(Interrupted { signal }) once recorded
pub(crate) struct LiveWorktree; // new(): live += 1; Drop: live -= 1; both under STATE
pub(crate) fn status(cmd: &mut Command) -> Result<ExitStatus>;
pub(crate) fn output(cmd: &mut Command) -> Result<Output>; // stdin null, pipes
pub fn reraise(signal: i32) -> ExitCode; // emulate_default_handler; else 128+signal

// src/worktree.rs: `live` stays the last field, so it covers all cleanup.
pub struct Worktree { /* T1 fields */ live: LiveWorktree }
```

Wiring:

- `run()` calls `interrupt::install()` before `dispatch`. On any `Err` it checks
  `interrupt::signal()` first and, if set, prints the error and returns
  `interrupt::reraise(signal)`. So neither a plumbing child killed by a tty
  Ctrl-C nor a sibling worker's lower-index error masks the signal.
- `Worktree::create` builds its `LiveWorktree` after the sweep and before the
  dir, lock or `add`. `git worktree add` runs through `interrupt::output()`; an
  interrupted add takes the add-failure remove.
- `run_subprocess`, `run_hk_validate` and `run_steps` spawn through the
  helpers; short git plumbing stays on plain `Command`.
- `run_once` calls `interrupt::check()?` beside each `cancel.is_cancelled()`
  checkpoint, so no fixup commit follows an interrupt.
- Each public hook entrypoint calls `interrupt::check()?` after its last
  worktree drops. A signal handled before the decrement is recorded and seen
  here; one after takes the default action. So no bookmark advance, push or
  `run` exit code follows an interrupt.

Tests (unix):

- `PRE_PUSH_SLEEPER`, then SIGTERM to jj-hp only; repeat with SIGINT. Assert:
  - jj-hp dies by that signal within 10s;
  - the root is empty;
  - the loop PID is gone (`ps -o stat= -p <pid>` is empty or `Z`);
  - remote `main` is unchanged;
  - there is no `refs/heads/jj-hooks-fixup/*` ref.

  jj-hp has no terminal here, so this exercises the group path.
- The same loop in a `jj-hooks.setup` step, for the captured path.
- A hook under `trap '' TERM` is alive 1s after one SIGTERM. A second SIGTERM
  kills it, and jj-hp dies by SIGTERM with an empty root.
- A `post-checkout` hook in the primary repo stalls `git worktree add`. On
  SIGTERM, jj-hp dies by it, the hook is gone, the root is empty and only the
  primary is listed.
- Linux, under `script -qec` (util-linux, added to devenv): the hook and a
  setup step record pgrp and session from `/proc/$$/stat`; each pair is equal.
  A SIGINT to that group, as Ctrl-C sends, makes `script` exit 130, and the
  root is empty.
- Linux: under `sh -c 'trap "" HUP; exec "$0" "$@"'`, a SIGHUP lets a run gated
  on a go-file finish with exit 0.
- Units (nextest runs each test in its own process):
  - `divert`: `live == 0` gives `Default` and records nothing; with
    `live == 1`, hits 1–3 give TERM, KILL, `Default`;
  - with `hits` preset, `status` returns `Interrupted` without spawning;
  - after the `waitid` NOWAIT step, `test_kill_process(pid)` is still `Ok`;
    after reaping, the registry is empty.

Red after T2: jj-hp dies at once and the checkout stays.

### T4 — docs and release

- `README.md`: replace "Hooks run in an ephemeral `/tmp` worktree". Document
  the root and its overrides, the sweep, and interrupt handling.
- Fix the `/tmp` doc comments: the `src/gate_cache.rs` and `src/repo_env.rs`
  module docs, and the `PklWarmCache` doc in `src/hooks.rs`.
- `CHANGELOG.md` `## [Unreleased]`: the root, interrupt cleanup, the sweep, the
  new `Worktree::create` signature, and a one-time `git worktree prune` for
  legacy `/tmp` entries. Bump to `0.4.0` (a public API break). Run
  markdownlint on the touched docs.
- Driver acceptance, after the release and the nix follow-up:
  - an hourly `df --output=pcent /tmp` over one fleet day peaks under 50%;
  - `<root>` holds no dir with an unlocked owner.

## Tasks

- [ ] T1 — on-disk root, sibling owner lock, `--force --force` remove;
  success and failure cleanup tests
- [ ] T2 — lock-proven `sweep`; SIGKILL-then-next-run and `sweep` tests
- [ ] T3 — `src/interrupt.rs`, `Interrupted`, re-raise in `run()`,
  live-before-acquire `create`, checkpoints; signal, escalation, add-stall, pty
  and SIGHUP tests
- [ ] T4 — README, doc comments, changelog, `0.4.0`, fleet-day `/tmp` check

## Open Questions

Each is designed against the stated assumption; none blocks T1.

1. **Deeper paths.** The checkout path grows from 29 to about 62 characters,
   nearer the 108-byte socket limit, and parent lookups (`~/.npmrc`) reach
   `$HOME`. Assumed acceptable, with `JJ_HOOKS_WORKTREE_ROOT` as the escape
   hatch. Alternative: a shorter default such as `~/.cache/jj-hp/wt`.
2. **Version.** Assumed `0.4.0` for the `Worktree::create` break. Alternative:
   `0.3.13`, if the lib API counts as internal.
3. **Non-Linux dispositions.** Off Linux, an inherited-ignored SIGINT or SIGTERM
   (a `&` job in a non-interactive shell) becomes catchable. Assumed
   acceptable: the fleet runs Linux. Alternative: watch only SIGTERM there, so
   a Ctrl-C leaks until the next sweep.
4. **Legacy admin entries.** jj-hp no longer touches lock-less entries, so old
   `/tmp` ones stay until a user prunes them. Assumed: a changelog note (T4).
   Alternative: reap missing-path entries under the canonical `temp_dir()` that
   git has not locked.
