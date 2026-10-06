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

Drop runs on return, `?` and panic unwind, but not on a fatal signal, and
nothing reaps leaks later. `TempDir::with_prefix("jj-hooks-worktree-")?` also
puts every worktree in RAM, through `std::env::temp_dir()`.

Intent:

- jj-hp removes its hook worktree on every exit it can observe.
- The next worktree creation reaps those whose owner died unobserved (SIGKILL,
  OOM, power loss).
- Worktrees live under an on-disk root.

Out of scope (nix-config follow-up): `NODE_COMPILE_CACHE`, `GOCACHE` and
`TMPDIR` on disk, and a systemd-tmpfiles age rule for legacy
`/tmp/jj-hooks-worktree-*` dirs.

## Global Constraints

- MSRV 1.89: std `File::lock`/`try_lock`, `LazyLock`, `process_group`.
- No `unsafe`: `signal-hook` for signals, `rustix` (feature `process`) for kill
  and wait, both under `[target.'cfg(unix)'.dependencies]`.
- Only `run()` (both binaries) installs signal handling. Without a live
  worktree a signal acts as today; on Linux an inherited ignore stays.
- Cleanup and sweep are best-effort: `tracing::warn!` and go on.
- An interrupt is an error, never a `Cancel` (which reports `success: true`).
- jj-hp never runs `git worktree prune`. It deletes a checkout or admin entry
  only on a dead-owner lock that it wrote.
- Keep `panic = "unwind"`. Write nothing inside the checkout
  (`maybe_build_fixup_commit` runs `git add -A` there).
- Code comments are 1–2 lines.

## Approach

Three layers: Drop (exists) for return, `?` and panic; signal handling (new),
which turns signals into `Interrupted` so Drop runs; a sweep (new) for SIGKILL,
OOM and power loss.

```text
<root>/jj-hooks-worktree-XXXXXX/      # the checkout (TempDir::with_prefix_in)
<root>/jj-hooks-worktree-XXXXXX.lock  # flock held for the Worktree's life; "pid\ngit_dir\n"
```

The checkout keeps the prefix as its basename, so git's admin entry does too.

New abstraction: `src/interrupt.rs`, process-global signal state; no module
has a writer outside the main flow.

### Fork 1 — cleanup on signal or kill

**Chosen:** keep the RAII Drop guard and add `signal-hook` handling; the Fork 2
sweep is the SIGKILL backstop.

Watched: SIGINT, SIGTERM and SIGHUP, minus any in the `/proc/self/status`
`SigIgn` mask at install, so `nohup` works. Other unix targets watch only
SIGINT and SIGTERM (Resolved decision 3).

Each gets two signal-hook actions, run in registration order:
`flag::register_usize` stores the signal in `PENDING` inside the handler, so
`check()` fails before any thread wakes; an iterator then wakes the watcher.

One mutex, `STATE`, holds the live count, hit count and child registry; the
watcher and `LiveWorktree` act only under it. Per signal, the watcher: with
`live == 0`, calls `emulate_default_handler(sig)`, as today; otherwise bumps
`hits` and sends each child (its group when `group`) SIGTERM on hit 1, SIGKILL
on hit 2, and the default action on hit 3. It forwards SIGTERM because many
runners read a second SIGINT as "force quit".

Spawn helpers keep this order (the PID-reuse invariant):

1. Under `STATE`: if `check()` fails, return `Interrupted`. Else spawn (with
   pipe readers if captured) and register `{pid, group}`, so the child sees
   every later escalation.
2. `waitid(Pid, EXITED | NOWAIT)`: wait for exit without reaping.
3. If captured: wait for the readers on a channel, polling `check()` every
   100ms, so a grandchild holding the pipe cannot block the abort.
4. If interrupted: when `group`, `kill_process_group(pid, KILL)` (the unreaped
   leader pins the PGID). Give the readers 1s, then detach any still blocked.
5. Under `STATE`: deregister. Then `child.wait()` reaps.
6. If `!group` and the child died by a watched SIGINT or SIGHUP, store it in
   `PENDING`: the terminal sent it to jj-hp too. Then `check()?`.

So a registered PID is always a live child or our zombie.

Groups: at install, jj-hp tries `File::open("/dev/tty")`. Without a controlling
terminal (fleet agents), every spawn gets `process_group(0)`, so group signals
reach grandchildren. With one, children stay in jj-hp's group: a background
group stops on SIGTTIN at a `/dev/tty` prompt, and Ctrl-C reaches them anyway.

Why not `PR_SET_PDEATHSIG`: it never removes the dir, is Linux-only, needs an
`unsafe` `pre_exec`, and fires when the spawning `thread::scope` worker exits.

Why not sweep alone: every Ctrl-C would leave 1.8G until the next run.

Stated limits:

- A descendant that leaves its group (`setsid`, a daemonizing build server)
  escapes both signals; paths it recreates after Drop have no lock, so the
  sweep keeps them. A subreaper or cgroup would fix this; out of scope.
- With a terminal, a grandchild ignoring SIGINT can defeat Drop's removal; the
  lock remains, so the sweep reaps the checkout later.
- With a terminal, a child that catches SIGINT and exits can be reaped before
  jj-hp's handler runs; the next spawn or checkpoint fails instead.

### Fork 2 — leak sweep

**Chosen:** an owner lock, not a PID. `create` makes the checkout dir and
`<name>.lock`, takes `File::lock()`, writes `pid\ngit_dir\n`, and calls
`sync_all()` on the lock and root dir (a root-dir error only warns) before
`git worktree add`. So a populated checkout's lock survives power loss. The
kernel drops a `flock` on any death, so `try_lock()` success means a dead
owner. The lock also proves ownership.

`sweep(root, grace)` runs once per process per root, at the first
`Worktree::create`. For each `jj-hooks-worktree-*` entry:

- Lock `WouldBlock`: the owner is live; skip.
- Lock taken and the content parses: run
  `git --git-dir=<g> worktree remove --force --force <dir>` (failure ignored),
  remove the dir if present, and unlink the lock. This also clears an orphan
  lock's admin entry.
- No lock, or an empty one: a creator may be mid-create, so skip while younger
  than `grace` (1h, by mtime). Then `remove_dir` (empty dirs only) and unlink
  the empty lock.
- A malformed lock: warn and skip.

`--force --force` removes the "initializing" lock that `add` sets
(git-worktree(1): "To remove a locked worktree, specify --force twice."). On
git 2.55 it drops a missing path's entry, also via a symlinked parent.

Why not prune, age or a PID marker: prune touches a user's worktree on an
unmounted disk; age deletes slow live gates or keeps leaks for hours; PID
reuse makes a PID verdict wrong both ways.

### Fork 3 — worktree root

**Chosen:** a configurable on-disk XDG cache root, with precedence like
`gate_cache_enabled`:

1. env `JJ_HOOKS_WORKTREE_ROOT` (absolute);
2. jj config `jj-hooks.worktree-root`;
3. `$XDG_CACHE_HOME/jj-hooks/worktrees` (if absolute);
4. `std::env::home_dir()` (which falls back to `getpwuid_r`) joined with
   `.cache/jj-hooks/worktrees`;
5. `std::env::temp_dir()/jj-hooks-worktrees`, with a tmpfs warning.

A relative override warns and falls through.

`~/.cache` shares `/home` with the bun cache, so its hardlinks
([bun install docs](https://bun.com/docs/pm/cli/install)) cost no RAM.
`temp_dir()` is tmpfs unless `TMPDIR` is set, a host choice for the nix
follow-up.

## Plan

Land T1 to T4 in order; release after T4. Each task's test cycle is
`cargo fmt -p jj-hooks -- --check`,
`cargo clippy -p jj-hooks --all-targets -- -D warnings` and
`cargo nextest run -p jj-hooks`. Each new test is red before its task.

### T1 — on-disk root and owner lock

`src/worktree.rs` gains the root resolver, the durable owner lock and a
`--force --force` remove (Drop and add failure). `src/hooks.rs` resolves the
root once at `run_for_update`, `run_for_updates_parallel` and
`run_for_partitioned_updates_parallel`.

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

New `tests/worktree_cleanup.rs`: `PRE_PUSH_RECORD_CWD`
(`sh -c 'printf "%s" "$PWD" > "$JJ_HOOKS_TEST_CWD_OUT"'`) and a failing twin
push with `JJ_HOOKS_WORKTREE_ROOT=<tmp>/root`. The cwd was under `<tmp>/root/`
and is gone, the root is empty, and the porcelain lists only the primary. Red
today. Units cover `root_from`; review checks the fsync order.

### T2 — sweep

```rust
const CREATION_GRACE: Duration = Duration::from_secs(3600);
// Reaps dead-owner checkouts under root; returns the reap count.
pub(crate) fn sweep(root: &Path, grace: Duration) -> usize;
```

A once-per-process gate (`static SWEPT: Mutex<HashSet<PathBuf>>`) calls it with
`CREATION_GRACE` at the top of `Worktree::create`.

Harness: `jj_hooks_spawn_with_env(&self, args, extra_env, log: &Path) -> Child`
runs jj-hp under `setsid` (util-linux, added to devenv), stdin null, output to
`log`. The child leads no group, so `setsid` execs in place: the PID is
jj-hp's, with no controlling terminal however nextest starts. Linux-only.

Fixture `PRE_PUSH_SLEEPER` records `$PWD` and its pgrp (field 5 of
`/proc/$$/stat`), starts `(while :; do date > tick; sleep 0.1; done) &` (it
holds stdout), records `$!` to `$JJ_HOOKS_TEST_PID_OUT`, and runs `wait`.

Tests:

- SIGKILL jj-hp and the loop once the cwd file exists, then push again with the
  same root: the root is empty and only the primary is listed. Red after T1.
- `sweep` units, zero grace: a held lock survives; an unlocked parseable lock
  is reaped with its admin entry, also under a symlinked root; an orphan
  parseable lock drops its admin entry; a lock-less
  `<root>/jj-hooks-worktree-user` worktree survives, still listed; an empty
  dir with an empty lock goes, but survives `CREATION_GRACE`.

### T3 — signal handling

```rust
// src/error.rs
#[error("interrupted by signal {signal}")]
Interrupted { signal: i32 },

// src/interrupt.rs: signal parts are #[cfg(unix)]; elsewhere install/check are no-ops.
static PENDING: LazyLock<Arc<AtomicUsize>> = LazyLock::new(Default::default); // set in the handler
static STATE: Mutex<State> = Mutex::new(State::new());
struct State { live: usize, hits: u32, children: Vec<Tracked> }
struct Tracked { pid: rustix::process::Pid, group: bool }
enum Action { Default, Send(rustix::process::Signal) }
// Pure watcher step: live == 0 → Default; else hit 1 TERM, 2 KILL, 3+ Default.
fn divert(state: &mut State, sig: i32) -> Action;
pub fn install(); // idempotent; PENDING is sticky, so long-lived hosts must not call it
pub fn signal() -> Option<i32>;
pub fn check() -> Result<()>; // Err(Interrupted { signal }) once PENDING is set
pub(crate) struct LiveWorktree; // new(): live += 1; Drop: live -= 1; both under STATE
pub(crate) fn status(cmd: &mut Command) -> Result<ExitStatus>;
pub(crate) fn output(cmd: &mut Command) -> Result<Output>; // stdin null, pipes
// output() minus steps 1 and 6's checks, so cleanup still runs after an interrupt.
pub(crate) fn cleanup_output(cmd: &mut Command) -> Result<Output>;
pub fn reraise(signal: i32) -> ExitCode; // emulate_default_handler; else 128+signal

// src/jj.rs: run() through cleanup_output.
pub fn run_cleanup(&self, args: &[&str]) -> Result<String>;

// src/worktree.rs: `live` stays the last field, so it covers all cleanup.
pub struct Worktree { /* T1 fields */ live: LiveWorktree }
```

Wiring:

- `run()` calls `interrupt::install()` before `dispatch`. On any `Err`, if
  `interrupt::signal()` is set, it returns `interrupt::reraise(signal)`, so no
  other error (a sibling worker's included) masks the signal.
- `Worktree::create` builds its `LiveWorktree` after the sweep, before the dir,
  lock or `add`.
- Every spawn while a worktree is live uses `status` or `output`:
  `git worktree add`, `run_subprocess`, `run_hk_validate`, `run_steps`, and
  plumbing (`run_git*`, `changed_files`, `JjCli::run_inner`,
  `git_local_env_vars`). Drop's remove, `delete_git_ref` and the fixup
  `jj bookmark forget` (via `run_cleanup`) use `cleanup_output`.
- `run_once` calls `interrupt::check()?` beside each `cancel.is_cancelled()`,
  and each public hook entrypoint after its last worktree drops. So no fixup,
  bookmark advance, push or `run` exit code follows an interrupt.

Tests (Linux, through the `setsid` harness unless stated):

- `PRE_PUSH_SLEEPER`, then SIGTERM to jj-hp only; repeat with SIGINT. Assert:
  - jj-hp dies by that signal within 10s;
  - the recorded pgrp differs from jj-hp's PID (the group path ran);
  - the root is empty, and the loop PID is gone (`ps -o stat=` empty or `Z`);
  - remote `main` is unchanged, with no `refs/heads/jj-hooks-fixup/*` ref.
- The same loop in a `jj-hooks.setup` step, for the captured path.
- Reader wake: a setup step backgrounds a TERM-ignoring loop, records its PID
  and exits 0. One SIGTERM: jj-hp dies by it within 10s; the loop is gone.
- Escalation: a setup step loops under `trap '' TERM`. It survives one SIGTERM
  for 1s; a second kills it and jj-hp.
- Add stall: a `post-checkout` hook in the primary repo stalls
  `git worktree add`. On SIGTERM, jj-hp dies by it, the hook is gone, and only
  the primary is listed.
- Terminal, under `script -qec` instead of `setsid`: the hook and a setup step
  record pgrp and session, and each pair is equal. A SIGINT to that group, as
  Ctrl-C sends, makes `script` exit 130.
- Under `sh -c 'trap "" HUP; exec "$0" "$@"'`, a SIGHUP lets a run gated on a
  go-file exit 0.
- Every signal test also asserts an empty root.
- Units (process-per-test under nextest): `divert` gives `Default` with
  `live == 0`, leaving `hits` 0, and TERM, KILL, `Default` for hits 1–3; with
  `PENDING` set, `status` refuses to spawn; after the NOWAIT step,
  `test_kill_process(pid)` is `Ok`, and after reaping the registry is empty.

Red after T2: jj-hp dies at once and the checkout stays.

### T4 — docs and release

- `README.md`: replace "Hooks run in an ephemeral `/tmp` worktree"; document
  the root, its overrides, the sweep, and interrupt handling with its limits.
- Fix the `/tmp` doc comments in the `gate_cache` and `repo_env` module docs
  and the `PklWarmCache` doc.
- `CHANGELOG.md` `## [Unreleased]`: the root, interrupt cleanup, the sweep, the
  `Worktree::create` signature, the non-Linux disposition change, and a
  one-time `git worktree prune` for legacy `/tmp` entries. Bump to `0.4.0`.
- Driver acceptance, after the release and the nix follow-up: an hourly
  `df --output=pcent /tmp` over one fleet day peaks under 50%, and `<root>`
  holds no dir with an unlocked owner.

## Tasks

- [ ] T1 — root, durable owner lock, double-force remove; cleanup tests
- [ ] T2 — lock-proven `sweep`, `setsid` harness; SIGKILL and `sweep` tests
- [ ] T3 — `src/interrupt.rs` and wiring; signal, reader-wake, escalation,
  add-stall, terminal and SIGHUP tests. Matt ruled to ship T3 (RIG-4482).
- [ ] T4 — README, doc comments, changelog, `0.4.0`, fleet-day `/tmp` check

## Resolved decisions

1. **Version `0.4.0`.** `Worktree::create` is public, and `release.yml` runs
   `cargo publish -p jj-hooks --locked`.
2. **Default root depth.** The path grows from 29 to about 62 characters
   (socket limit 108), and parent lookups (`~/.npmrc`) reach `$HOME`. Kept:
   `JJ_HOOKS_WORKTREE_ROOT` is the escape hatch, and a later default change
   touches no interface.
3. **Non-Linux dispositions.** Off Linux, an inherited-ignored SIGINT or SIGTERM
   (a `&` job in a script) becomes catchable. Accepted and documented (T4):
   watching only SIGTERM there would leak 1.8G per Ctrl-C.
4. **Legacy admin entries.** jj-hp leaves lock-less `/tmp` entries alone. The
   changelog gives a one-time `git worktree prune`; otherwise `git gc` prunes
   them after `gc.worktreePruneExpire` (3 months by default, git-config(1)).

## Open Questions

1. **T3 scope.** Matt ruled to ship T3 (RIG-4482).
