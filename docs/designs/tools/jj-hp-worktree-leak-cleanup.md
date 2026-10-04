# Design: jj-hp hook worktrees — leak-proof cleanup

Status: **proposed**
Domain: tools

Tracking issue: RIG-4294. Crate: `jj-hooks` (this repo, the `jj-hp` binary),
currently `0.3.12`.

## Problem / Intent

On mattfw (2026-10-04) the shared `/tmp` tmpfs (63G) reached 100%, and jj-hp
push hooks failed with ENOSPC. Eight `/tmp/jj-hooks-worktree-*` dirs held
1.3–1.9G each, mostly `node_modules` with link count 1: the bun cache is on
`/home` btrfs and cannot hardlink into tmpfs. Two had leaked. No process had a
cwd or fd in them, yet their admin dirs under `<repo>/.git/worktrees/` remained,
with no `locked` file. Another 11G was `/tmp/node-compile-cache` (host env).

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

Drop runs on return, `?` and panic unwind. It does not run when SIGINT, SIGTERM
or SIGHUP kills the process by default action, or on SIGKILL, and nothing reaps
the leak later. Also, `TempDir::with_prefix("jj-hooks-worktree-")?` resolves
through `std::env::temp_dir()`, so every worktree lands in RAM.

Intent:

- jj-hp removes its hook worktree on every exit path it can observe.
- The next jj-hp run that creates a hook worktree reaps worktrees whose owner
  died unobserved (SIGKILL, OOM, power loss).
- Worktrees move off tmpfs to an on-disk root.

Out of scope, as a nix-config follow-up: `NODE_COMPILE_CACHE`, `GOCACHE` and
`TMPDIR` on disk, and a systemd-tmpfiles age rule for legacy
`/tmp/jj-hooks-worktree-*` dirs.

## Global Constraints

- MSRV 1.89: std `File::lock`/`try_lock`, `IsTerminal` and
  `CommandExt::process_group`. No lock crate.
- No `unsafe`. Signals go through `signal-hook` (an iterator thread); kill and
  wait go through `rustix` (feature `process`). Both go under
  `[target.'cfg(unix)'.dependencies]`; non-unix keeps plain `Command`.
- Only `run()` (both binaries) installs signal handling. Library callers keep
  default signal behaviour unless they call `interrupt::install()`.
- With no live worktree, a signal takes its default action, as today. On Linux
  a signal ignored at startup stays ignored (Open Question 3 covers the rest).
- Cleanup, sweep and entry removal are best-effort: `tracing::warn!` and go on.
- An interrupt is an error, never a cancellation. `Cancel` short-circuits "with
  `success: true`" (its doc in `src/hooks.rs`); an interrupted run must abort.
- jj-hp never runs `git worktree prune`. It removes only admin entries it can
  prove it owns.
- Release keeps the default `panic = "unwind"`; Drop on unwind is part of the
  contract.
- Nothing is written inside the checkout (`maybe_build_fixup_commit` runs
  `git add -A` there).
- Code comments are 1–2 lines.

## Approach

Three layers, each covering exits the one above cannot: Drop (exists) for
return, `?` and panic; a signal watcher (new) that turns signals into an
`Interrupted` error so Drop runs; and a sweep (new) for SIGKILL, OOM and
crashes.

Layout under a configurable on-disk root:

```text
<root>/jj-hooks-worktree-XXXXXX/      # the checkout (TempDir::with_prefix_in)
<root>/jj-hooks-worktree-XXXXXX.lock  # flock held for the Worktree's life; "pid\ngit_dir\n"
```

The checkout keeps the prefix as its basename, so git's admin entry stays
`<git>/worktrees/jj-hooks-worktree-XXXXXX`. The lock is a sibling of the
checkout, never inside it.

New abstraction: `src/interrupt.rs` holds process-global state that a watcher
thread writes and spawn sites read. The per-workspace `gate_cache`/`repo_env`
caches have no writer thread.

### Fork 1 — cleanup on signal or kill

**Chosen:** keep the RAII Drop guard and add a `signal-hook` watcher thread.
Signal children through a registry that never names a reaped PID. Give each
child its own process group unless it shares jj-hp's terminal. The Fork 2 sweep
is the SIGKILL backstop.

Watched signals: SIGINT, SIGTERM and SIGHUP, minus any whose bit is set in the
`SigIgn` line of `/proc/self/status` at install, so `nohup` keeps working.
Other unix targets cannot read the disposition without `unsafe`, so there only
SIGINT and SIGTERM are watched.

For each signal, the watcher:

- `LIVE == 0`: calls `emulate_default_handler(sig)`, as today.
- Otherwise: records the first signal in `SIGNAL` and bumps `HITS`. Under the
  `CHILDREN` lock it signals each child (its group when `group`): SIGTERM on
  hit 1, SIGKILL on hit 2. Hit 3 calls `emulate_default_handler(sig)`.

It forwards SIGTERM because many runners read a second SIGINT as "force quit".

Spawn helpers keep this order, which is the PID-reuse invariant:

1. `check()?`.
2. Under the `CHILDREN` lock: spawn (start pipe readers if captured), register
   `{pid, group}`, and send SIGTERM if `HITS > 0`.
3. `waitid(Pid, EXITED | NOWAIT)`: wait for exit without reaping.
4. If captured: join the readers, but stop once interrupted, so a grandchild
   holding the pipe cannot block the abort.
5. If interrupted and `group`: `kill_process_group(pid, KILL)`. The unreaped
   leader pins the PGID.
6. Under the lock: deregister. Then `child.wait()` reaps.
7. `check()?`.

So a registered PID is always a live child or our zombie, never a reused PID.

Groups: captured spawns always get `process_group(0)`. Live spawns get one only
when none of jj-hp's stdio `is_terminal()` (jj-vine pipes jj-hp). A group
signal reaches grandchildren, so stragglers die before Drop removes the
checkout. On a terminal, children stay in the foreground group, where Ctrl-C
already reaches every descendant. There the guarantee narrows to the direct
child, and a straggler that defeats removal is left to the sweep.

Why not a group for every spawn: a background-group child gets SIGTTIN/SIGTTOU
on terminal I/O, and tty Ctrl-C stops reaching it.

Why not `PR_SET_PDEATHSIG`: it never removes the dir, is Linux-only
(darwin-arm64 is a release target), needs an `unsafe` `pre_exec`, and fires
when the spawning thread exits (a `thread::scope` worker here).

Why not sweep alone: every Ctrl-C would leave 1.8G until the next run.

### Fork 2 — leak sweep

**Chosen:** an owner lock, not an owner PID. `create` makes the checkout dir and
creates `<name>.lock`. It takes `File::lock()`, writes `pid\ngit_dir\n`, and
only then runs `git worktree add`. The kernel drops a `flock` on any death,
SIGKILL included. So when `try_lock()` succeeds, the owner is dead.

`sweep(root)` runs once per process per root, at the first `Worktree::create`.
For each `jj-hooks-worktree-*` dir:

- Lock `WouldBlock`: the owner is live, so skip.
- Unlocked and the content parses: run
  `git --git-dir=<g> worktree remove --force --force <dir>`, then
  `remove_dir_all`, then unlink the lock.
- Lock missing, or unlocked with empty or malformed content: a creator may be
  mid-create, so skip while younger than `CREATION_GRACE` (1h, by mtime). After
  that, run `remove_dir_all` and unlink.

An orphan `.lock` with no dir is unlinked once `try_lock` succeeds.

`git worktree prune` would touch every worktree, including a user's on an
unmounted disk. Instead, `remove_owned_entries` runs once per process per
primary git dir, under `WORKTREE_CREATE_LOCK`. From
`git worktree list --porcelain` it removes (`--force --force`) only entries
whose basename has the prefix, whose parent is the root or
`std::env::temp_dir()` (legacy), and whose path is missing. A live creator's
path always exists, because the dir precedes `add`. Measured on git 2.55:
removing a missing path, locked or not, exits 0 and drops the entry.

`--force --force`: `add` locks its entry as "initializing", and git-worktree(1)
says "To remove a locked worktree, specify --force twice." Drop uses it too.

Why not age-based removal: it deletes slow live gates or keeps leaks for hours.

Why not a PID marker: PID reuse can make the verdict wrong in both directions.

### Fork 3 — worktree root

**Chosen:** an on-disk XDG cache root that the user can configure. The
precedence mirrors `gate_cache_enabled`:

1. env `JJ_HOOKS_WORKTREE_ROOT` (absolute);
2. jj config `jj-hooks.worktree-root`;
3. `$XDG_CACHE_HOME/jj-hooks/worktrees` (if absolute);
4. `std::env::home_dir()` (it falls back to `getpwuid_r` when `HOME` is unset)
   joined with `.cache/jj-hooks/worktrees`;
5. `std::env::temp_dir()/jj-hooks-worktrees`, only when no home dir is known.
   It warns that the root may be tmpfs.

A relative override warns and falls through to the next step.

On mattfw, `findmnt` shows `~/.cache` and `~/.bun/install/cache` on the same
`/home` btrfs. Bun's Linux install backend is hardlink
([bun install docs](https://bun.com/docs/pm/cli/install)), so `node_modules`
costs inodes, not 1.7G of RAM. A dedicated root also bounds what the sweep
scans.

Why not `std::env::temp_dir()`: it is tmpfs unless `TMPDIR` is set. Setting
`TMPDIR` is a host-wide choice that belongs to the nix follow-up.

## Plan

Land T1 to T4 in order, and release after T4. The test cycle for each task is
`cargo fmt -p jj-hooks -- --check`,
`cargo clippy -p jj-hooks --all-targets -- -D warnings` and
`cargo nextest run -p jj-hooks`. Each new test is red before its task.

### T1 — on-disk root and owner lock

`src/worktree.rs`: the root resolver, the sibling owner lock, and a
`--force --force` remove (used by Drop and by the add-failure path).

`src/hooks.rs`: resolve the root once at `run_for_update`,
`run_for_updates_parallel` and `run_for_partitioned_updates_parallel`, then pass
it to `Worktree::create` in `run_once`.

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

New test file `tests/worktree_cleanup.rs`, with two fixtures:

- `PRE_PUSH_RECORD_CWD`:
  `sh -c 'printf "%s" "$PWD" > "$JJ_HOOKS_TEST_CWD_OUT"'`;
- a failing twin that ends with `exit 1`.

For each fixture, push with `JJ_HOOKS_WORKTREE_ROOT=<tmp>/root`, then assert:

- the recorded cwd is under `<tmp>/root/` and no longer exists;
- `<tmp>/root` is empty;
- `git worktree list --porcelain` lists only the primary.

These are red today, because the cwd is under `/tmp`. Unit tests cover
`root_from`, including a relative override falling through and the no-home
fallback.

### T2 — sweep and owned-entry removal

```rust
const CREATION_GRACE: Duration = Duration::from_secs(3600);
// Reaps dead-owner checkouts under root; returns the reap count.
pub(crate) fn sweep(root: &Path) -> usize;
// Removes missing, prefixed entries under root or temp_dir; never prunes.
fn remove_owned_entries(git_dir: &Path, root: &Path);
```

Once-per-process gates (`static SWEPT` and `static CLEANED`, both
`Mutex<HashSet<PathBuf>>`) run at the top of `Worktree::create`.

Harness: `jj_hooks_spawn_with_env(&self, args, extra_env, log: &Path) -> Child`
(stdin null, output to `log`). Fixture `PRE_PUSH_SLEEPER` records `$PWD`, starts
`(while :; do date > tick; sleep 0.1; done) &` (it holds stdout), records `$!`
to `$JJ_HOOKS_TEST_PID_OUT`, and runs `wait`.

Tests:

- SIGKILL jj-hp and the loop once the cwd file exists, then push again with the
  same root. The root is empty and only the primary is listed. Red after T1.
- An unrelated `git worktree add <tmp>/user-wt` whose dir is deleted stays
  listed after a push.
- `sweep` units: a held lock survives; an unlocked parseable lock is reaped; a
  fresh empty lock survives; a stale malformed lock is reaped; unprefixed
  entries are untouched.

### T3 — signal watcher

```rust
// src/error.rs
#[error("interrupted by signal {signal}")]
Interrupted { signal: i32 },

// src/interrupt.rs: signal parts are #[cfg(unix)]; elsewhere install/check are no-ops.
static SIGNAL: AtomicI32 = AtomicI32::new(0); // first diverted signal; 0 = none
static HITS: AtomicU32 = AtomicU32::new(0); // 1 TERM, 2 KILL, 3+ default action
static LIVE: AtomicUsize = AtomicUsize::new(0);
static CHILDREN: Mutex<Vec<Tracked>> = Mutex::new(Vec::new());
struct Tracked { pid: rustix::process::Pid, group: bool }
// Idempotent. The flag is sticky: hosts that outlive an interrupt must not call it.
pub fn install();
pub fn check() -> Result<()>; // Err(Interrupted { signal: SIGNAL }) once set
pub(crate) struct LiveWorktree; // new(): LIVE += 1; Drop: LIVE -= 1
pub(crate) fn status(cmd: &mut Command) -> Result<ExitStatus>;
pub(crate) fn output(cmd: &mut Command) -> Result<Output>; // stdin null, pipes
pub fn reraise(signal: i32) -> ExitCode; // emulate_default_handler; else 128+signal

// src/worktree.rs: `live` stays the last field, so LIVE covers all cleanup.
pub struct Worktree { /* T1 fields */ live: LiveWorktree }
```

Wiring:

- `run()` calls `interrupt::install()` before `dispatch`. It maps
  `Err(JjHooksError::Interrupted { signal })` to a message plus
  `interrupt::reraise(signal)`.
- `run_subprocess`, `run_hk_validate` and `run_steps` spawn through the
  helpers. Short git plumbing calls stay on plain `Command`.
- `run_once` calls `interrupt::check()?` beside each `cancel.is_cancelled()`
  checkpoint, so no fixup commit is built after an interrupt.
- Each public hook entrypoint calls `interrupt::check()?` after its last
  worktree drops. Any later signal sees `LIVE == 0` and takes the default
  action. So no bookmark advance, push, or `run` exit code follows an
  interrupt.

Tests (unix):

- `PRE_PUSH_SLEEPER`, then SIGTERM to jj-hp only; repeat with SIGINT. Assert:
  - jj-hp dies by that signal within 10s;
  - the root is empty;
  - the loop PID is gone (`ps -o stat= -p <pid>` is empty or `Z`);
  - remote `main` is unchanged;
  - there is no `refs/heads/jj-hooks-fixup/*` ref.

  jj-hp has no terminal here, so this exercises the group path.
- The same loop inside a `jj-hooks.setup` step, for the captured path.
- Linux only: under `sh -c 'trap "" HUP; exec "$0" "$@"'`, a SIGHUP lets a run
  gated on a go-file finish with exit 0.
- Unit: after the `waitid` NOWAIT step, `test_kill_process(pid)` is still `Ok`
  (the zombie pins the PID). After reaping, `CHILDREN` is empty.

These are red after T2: jj-hp dies at once and the checkout stays.

### T4 — docs and release

- `README.md`: replace "Hooks run in an ephemeral `/tmp` worktree". Document
  the root and its overrides, the sweep, and interrupt handling.
- Fix the `/tmp` doc comments: the `src/gate_cache.rs` and `src/repo_env.rs`
  module docs, and the `PklWarmCache` doc in `src/hooks.rs`.
- `CHANGELOG.md` `## [Unreleased]`: the root, interrupt cleanup, the sweep, and
  the new `Worktree::create` signature. Bump to `0.4.0` (a public API break).
  Run markdownlint on the touched docs.
- Driver acceptance, after the release and the nix follow-up:
  - an hourly `df --output=pcent /tmp` over one fleet day peaks under 50%;
  - `<root>` holds no dir with an unlocked owner.

## Tasks

- [ ] T1 — `worktree_root`/`root_from`, sibling owner lock, `--force --force`
  remove, root threaded through the hook entrypoints; red→green tests for a
  successful and a failed run
- [ ] T2 — `sweep` (flock liveness, grace for missing or malformed locks) and
  `remove_owned_entries`; SIGKILL-then-next-run, unrelated-worktree and `sweep`
  tests
- [ ] T3 — `src/interrupt.rs` (watcher, registry, helpers, groups),
  `Interrupted`, install and re-raise in `run()`, checkpoint and entrypoint
  checks; signal, grandchild, setup-step and SIGHUP-ignore tests
- [ ] T4 — README; `gate_cache`, `repo_env` and `PklWarmCache` docs; changelog;
  `0.4.0`; fleet-day `/tmp` check

## Open Questions

Each one is designed against the stated assumption. None blocks T1.

1. **Deeper paths.** The checkout path grows from 29 to about 62 characters.
   That is closer to the 108-byte unix-socket limit, and parent-directory
   lookups (`~/node_modules`, `~/.npmrc`) now reach `$HOME`.
   - Assumed: acceptable, with `JJ_HOOKS_WORKTREE_ROOT` as the escape hatch.
   - Alternative: a shorter default, such as `~/.cache/jj-hp/wt`.
2. **Version.**
   - Assumed: `0.4.0`, for the `Worktree::create` break.
   - Alternative: `0.3.13`, if the lib API counts as internal.
3. **Non-Linux dispositions.** Off Linux, an inherited-ignored SIGINT or
   SIGTERM (for example, a `&` job in a non-interactive shell) becomes
   catchable.
   - Assumed: accept it; the fleet runs Linux.
   - Alternative: watch only SIGTERM off Linux. A Ctrl-C there would then leak
     until the next sweep.
