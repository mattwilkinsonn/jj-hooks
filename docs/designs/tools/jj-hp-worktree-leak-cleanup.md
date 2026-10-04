# Design: jj-hp hook worktrees — leak-proof cleanup

Status: **proposed**
Domain: tools

Tracking issue: RIG-4294. Crate: `jj-hooks` (this repo, the `jj-hp` binary),
currently `0.3.12`.

## Problem / Intent

On mattfw (2026-10-04) the shared `/tmp` tmpfs (63G) reached 100%, and jj-hp
push hooks failed with ENOSPC. Eight `/tmp/jj-hooks-worktree-*` dirs held
1.3–1.9G each. Two had leaked: no process had a cwd or fd in them, yet their
git admin dirs (`<repo>/.git/worktrees/jj-hooks-worktree-*`) were still there,
with no `locked` file. So the owner died after `git worktree add` finished.
Most of each worktree is `node_modules` (1.7G) with link count 1: the bun cache
is on `/home` btrfs and cannot hardlink into tmpfs. Another 11G was
`/tmp/node-compile-cache`, which is host env.

`run_once` in `src/hooks.rs` creates the worktree and then blocks on hook
children:

```rust
let wt = Worktree::create(primary_git_dir, target_commit)?;
```

Cleanup is one Drop impl in `src/worktree.rs`:

```rust
impl Drop for Worktree {
    fn drop(&mut self) {
        if let Err(e) = self.remove() {
```

Drop runs on a normal return, on a `?` error, and on panic unwind. It does not
run when SIGINT, SIGTERM or SIGHUP kills the process by default action, or on
SIGKILL. Nothing reaps such a leak later. `Worktree::create` also puts every
worktree on tmpfs (RAM), because
`TempDir::with_prefix("jj-hooks-worktree-")?` resolves through
`std::env::temp_dir()`.

Intent:

- jj-hp removes its hook worktree on every exit path it can observe.
- The next jj-hp run on the host reaps any worktree whose owner died
  unobserved (SIGKILL, OOM, power loss).
- Worktrees move off tmpfs to an on-disk root.

Out of scope, as a separate nix-config follow-up:

- Put `NODE_COMPILE_CACHE`, `GOCACHE` and `TMPDIR` on disk.
- Add a systemd-tmpfiles age rule for the legacy `/tmp/jj-hooks-worktree-*`
  dirs that pre-fix binaries leave.

## Global Constraints

- MSRV stays 1.89 (`rust-version` in `Cargo.toml`). Use std `File::lock` and
  `File::try_lock` (stable since 1.89), not a lock crate.
- No `unsafe` in jj-hooks:
  - Signals go through `signal-hook` (an iterator thread, not a raw handler).
  - Kills go through `rustix::process::kill_process` (feature `process`).
  - Both deps go under `[target.'cfg(unix)'.dependencies]`. Non-unix builds
    keep plain `Command::status`/`output`.
- Cleanup and sweep are best-effort. A failure warns once with
  `tracing::warn!`, and the gate goes on. Only an interrupt aborts a run.
- An interrupt is an error, never a cancellation. `Cancel` short-circuits "with
  `success: true`" (its doc in `src/hooks.rs`), but an interrupted run must
  abort the push.
- With no live worktree, signal behaviour stays as it is today: the process
  dies by the default action.
- The release profile keeps the default `panic = "unwind"`. Drop on unwind is
  part of the cleanup contract.
- Never write inside the checkout. `maybe_build_fixup_commit` runs
  `git add -A` there, so owner metadata goes beside the checkout.
- Code comments are 1–2 lines.

## Approach

Three layers, each covering exits the one above cannot:

1. **Drop** (exists): normal return, `?`, panic.
2. **Signal watcher** (new): turns SIGINT, SIGTERM and SIGHUP into an
   `Interrupted` error, so layer 1 runs.
3. **Sweep** (new): covers SIGKILL, OOM and crashes. The next run reaps any
   container whose owner is dead.

Each worktree gets one container under a configurable on-disk root:

```text
<root>/jj-hooks-worktree-XXXXXX/   # TempDir::with_prefix_in container
  owner.lock                       # flock held for the Worktree's life; "pid\ngit_dir\n"
  wt/                              # the git checkout (Worktree::path)
```

New abstraction: `src/interrupt.rs` holds process-global signal state: the
interrupted flag, the live-worktree count and the child-PID registry. The
`gate_cache`/`repo_env` caches cannot carry it. They are per-workspace and
only read at spawn sites, but a signal thread writes this state.

### Fork 1 — cleanup on signal or kill

**Chosen:** keep the RAII Drop guard and add a `signal-hook` iterator thread
for SIGINT, SIGTERM and SIGHUP. Forward signals to direct children only, with
no new process group. The Fork 2 sweep is the SIGKILL backstop.

The watcher diverts a signal only while `LIVE > 0`. Otherwise it calls
`emulate_default_handler(sig)`, which is today's behaviour. While a worktree is
live:

- First signal: set the flag and send SIGTERM to every registered child.
- Second signal: send SIGKILL to every registered child.
- Third signal: call `emulate_default_handler(sig)`. The sweep reaps later.

Each spawn helper registers the child PID, waits, deregisters, and then
returns `Err(Interrupted)` if the flag is set. `run_once` unwinds through `?`,
and the worktree drops. `run()` re-raises the signal, so the parent sees a
death by signal. The watcher forwards SIGTERM, not the signal it got: a tty
Ctrl-C has already sent SIGINT to the foreground group, and many runners read
a second SIGINT as "force quit".

Why not a process group per child (`process_group(0)` with `killpg`): it would
reach grandchildren, but a background-group child gets SIGTTIN/SIGTTOU on
terminal I/O (hk's live progress bar), and tty Ctrl-C stops reaching it. An
orphaned grandchild only holds an unlinked cwd.

Why not `PR_SET_PDEATHSIG`:

- It kills children but never removes the directory.
- It is Linux-only; darwin-arm64 is a release target.
- It needs an `unsafe` `pre_exec`.
- It fires when the spawning *thread* exits: a `thread::scope` worker here.

Why not sweep alone: every Ctrl-C would leave 1.8G until the next run.

### Fork 2 — leak sweep

**Chosen:** an owner **lock**, not an owner PID. `create` writes `owner.lock`,
takes `File::lock()` on it, and then writes `pid\ngit_dir\n` while it holds the
lock. The lock is held for the `Worktree`'s whole life. The kernel drops a
`flock` when its process dies, even on SIGKILL. So if `try_lock()` succeeds,
the owner is dead. No PID-reuse or clock rule is needed.

`sweep(root)` runs synchronously, once per process per root, at the first
`Worktree::create`. For each `jj-hooks-worktree-*` entry:

- `try_lock` returns `WouldBlock`: the owner is live, so skip it.
- The lock is taken and the content parses: reap it, holding the lock so other
  sweepers skip it. Run `git --git-dir=<g> worktree remove --force --force
  <c>/wt` (ignore a failure), then `remove_dir_all(c)`.
- `owner.lock` is missing or empty, and younger than `CREATION_GRACE` (1h): a
  creator is between mkdir and lock, so skip. If it is older, run
  `remove_dir_all(c)`. The prune below clears its admin entry.

Next, run `git worktree prune --expire=1.hour.ago` once per process per
primary git dir, under `WORKTREE_CREATE_LOCK`. This clears admin entries whose
checkout is gone, such as legacy `/tmp` leaks after tmpfiles removes them. The
expiry keeps prune away from another process's `worktree add` in flight.

`--force --force`: `git worktree add` locks its entry as "initializing" while
it runs, and git-worktree(1) says "To remove a locked worktree, specify --force
twice." Drop and the add-failure path use it too.

Normal-path cost: one `readdir` and one `try_lock` per live container.

Why not age-based removal: it either deletes a slow live gate or holds a leak
for hours. With an on-disk root, a reboot no longer wipes the leak either.

Why not a PID marker: PID reuse makes the verdict wrong both ways. The PID is
in the file for humans only.

### Fork 3 — worktree root

**Chosen:** an on-disk XDG cache root that the user can configure. The
precedence mirrors `gate_cache_enabled`:

1. env `JJ_HOOKS_WORKTREE_ROOT` (non-empty and absolute);
2. jj config `jj-hooks.worktree-root`;
3. `$XDG_CACHE_HOME/jj-hooks/worktrees` (if absolute);
4. `$HOME/.cache/jj-hooks/worktrees`;
5. `std::env::temp_dir().join("jj-hooks-worktrees")` when `HOME` is unset.

A relative or unusable override warns and falls through to the next step.

On mattfw, `findmnt` shows `~/.cache` and `~/.bun/install/cache` on the same
`/home` btrfs. Bun's default Linux install backend is hardlink
([bun install docs](https://bun.com/docs/pm/cli/install): "clonefile on macOS
and hardlink on Linux"). So a worktree's `node_modules` costs inodes, not
1.7G, and neither the files nor their removal touch RAM. A dedicated root also
means the sweep scans only a directory that jj-hp owns.

Why not `std::env::temp_dir()`: it is tmpfs (RAM) unless `TMPDIR` is set, and
setting `TMPDIR` moves every tool's temp files host-wide. That is the nix
follow-up's call.

## Plan

The four tasks ship as one release after T4, and land in order: T2 and T3
build on T1's container layout.

Test cycle for every task: `cargo fmt -p jj-hooks -- --check`,
`cargo clippy -p jj-hooks --all-targets -- -D warnings`,
`cargo nextest run -p jj-hooks`. Each new test is red before its task.

### T1 — on-disk root and container layout

`src/worktree.rs`:

- the root resolver;
- the container with `owner.lock` and `wt/`;
- a `--force --force` remove, used by Drop and by `create` when
  `git worktree add` fails.

`src/hooks.rs`: resolve the root once at `run_for_update`,
`run_for_updates_parallel` and `run_for_partitioned_updates_parallel`, then
pass it down to `Worktree::create` in `run_once`.

Interfaces:

```rust
// src/worktree.rs
pub const WORKTREE_PREFIX: &str = "jj-hooks-worktree-";
pub fn worktree_root(jj: &crate::jj::JjCli) -> PathBuf;
// Pure precedence core of `worktree_root`; unit-tested.
fn root_from(env: Option<&OsStr>, config: Option<&str>, xdg_cache: Option<&OsStr>, home: Option<&OsStr>) -> PathBuf;
// Field order matters: `container` drops before `owner`, so the lock outlives the dir.
pub struct Worktree { git_dir: PathBuf, container: TempDir, checkout: PathBuf, owner: File, removed: bool }
impl Worktree {
    pub fn create(root: &Path, git_dir: &Path, commit: &str) -> Result<Self>; // create_dir_all(root) first
    pub fn path(&self) -> &Path; // <container>/wt
    pub fn git_dir(&self) -> &Path;
}
// Runs under WORKTREE_CREATE_LOCK.
fn git_worktree_remove(git_dir: &Path, checkout: &Path) -> std::io::Result<()>;
// src/hooks.rs (private): run_for_update_with_cancel and run_once gain `root: &Path`.
```

Callers: the two `Worktree::create` calls in `tests/workspace.rs` pass a root
inside the test's tmp dir.

Tests go in a new `tests/worktree_cleanup.rs` (`mod harness;`), with these
harness fixtures:

- `PRE_PUSH_RECORD_CWD`:
  `sh -c 'printf "%s" "$PWD" > "$JJ_HOOKS_TEST_CWD_OUT"'`, exits 0.
- `PRE_PUSH_RECORD_CWD_FAILING`: the same, then `exit 1`.

For each fixture, push `main` with `JJ_HOOKS_WORKTREE_ROOT=<tmp>/root`, then
assert:

- the recorded cwd is under `<tmp>/root/` and no longer exists;
- `<tmp>/root` is empty;
- `git worktree list --porcelain` in the primary lists only the primary.

These are red today, because the cwd is under `/tmp`. Unit tests cover the
`root_from` precedence, including a relative override falling through.

### T2 — leak sweep and prune

`src/worktree.rs`: add `sweep` and its once-per-process gates
(`static SWEPT: Mutex<HashSet<PathBuf>>`, `static PRUNED: Mutex<HashSet<PathBuf>>`).
Call them at the top of `Worktree::create`.

Interfaces:

```rust
const CREATION_GRACE: Duration = Duration::from_secs(3600);
// Reaps containers whose owner.lock is unlocked; returns the reap count.
pub(crate) fn sweep(root: &Path) -> usize;
// git worktree prune --expire=1.hour.ago; warns on failure.
fn prune_once(git_dir: &Path);
```

Harness additions:

- `jj_hooks_spawn_with_env(&self, args, extra_env, log: &Path) -> std::process::Child`.
  Stdin is null. Stdout and stderr go to `log`, so an orphaned grandchild
  cannot hold the test's pipe open.
- Fixture `PRE_PUSH_RECORD_CWD_THEN_SLEEP`: writes `$PWD` to the cwd file and
  `$$` to `$JJ_HOOKS_TEST_PID_OUT`, then runs `exec sleep 60`.

Integration test:

1. Spawn jj-hp with the sleep fixture and poll for the cwd file.
2. `kill -KILL` jj-hp and the sleep PID.
3. Run a passing push with the same root.
4. Assert the first container is gone, and that
   `git worktree list --porcelain` lists only the primary.

This is red after T1, because nothing reaps the container.

Unit tests (`sweep`):

- a container whose `owner.lock` the test holds with `File::lock` survives;
- an unlocked container with content is reaped;
- an empty, fresh `owner.lock` survives;
- an entry without the prefix is untouched.

### T3 — signal watcher

Changes:

- New module `src/interrupt.rs`; its signal parts are `#[cfg(unix)]`.
- New error variant `JjHooksError::Interrupted`.
- `run_subprocess` and `run_hk_validate` (`src/hooks.rs`) and `run_steps`
  (`src/setup.rs`) spawn through the new helpers. Short git plumbing calls
  stay on plain `Command`.

Interfaces:

```rust
// src/error.rs
#[error("interrupted by signal {signal}")]
Interrupted { signal: i32 },

// src/interrupt.rs
pub fn install(); // idempotent (OnceLock); spawns the watcher thread
pub fn check() -> Result<()>; // Err(Interrupted) once a signal has landed
pub struct LiveWorktree; // new() increments LIVE, Drop decrements; last Worktree field
pub fn status(cmd: &mut Command) -> Result<ExitStatus>;
// Sets stdin null and stdout/stderr piped, as Command::output does.
pub fn output(cmd: &mut Command) -> Result<Output>;
pub fn reraise(signal: i32) -> ExitCode; // emulate_default_handler; 128+signal if it returns
```

Each helper runs these steps in order:

1. `check()`.
2. `spawn`, then register the PID.
3. If the flag is already set, forward SIGTERM.
4. `wait` or `wait_with_output`, then deregister the PID.
5. `check()` again.

Wiring:

- `Worktree::create` calls `install()` and `check()` before it creates
  anything, and builds `LiveWorktree` first.
- `dispatch` in `src/lib.rs` calls `interrupt::check()?` before each
  `execute_push`, so a signal that lands after the last spawn still aborts.
- `run()` matches `Err(JjHooksError::Interrupted { signal })`, prints it, and
  returns `interrupt::reraise(signal)`.

Tests (`tests/worktree_cleanup.rs`, unix only): spawn with the sleep fixture,
wait for the cwd file, then send `kill -TERM` to jj-hp only. Assert:

- jj-hp exits by signal 15 (`ExitStatusExt::signal`);
- the root is empty;
- no push reached the remote (`remote_commit("main")` is unchanged).

Repeat with SIGINT. This is red after T2: jj-hp dies at once and the container
stays.

### T4 — docs and release

Docs:

- `README.md`: rewrite the "Hooks run in an ephemeral `/tmp` worktree"
  sentence. Add a section on the root, its override, the sweep and the prune
  side effect.
- Doc comments that say the worktree is under `/tmp`: the `src/gate_cache.rs`
  and `src/repo_env.rs` module docs, and the `PklWarmCache` doc in
  `src/hooks.rs`.

`CHANGELOG.md`, under `## [Unreleased]`: the on-disk root, interrupt cleanup,
the sweep, and the new `Worktree::create` signature. Bump to `0.4.0`, because
this is a public API break before 1.0. Run markdownlint on the docs you touch.

Driver acceptance, after the release and the nix follow-up:

- Sample `df --output=pcent /tmp` every hour for one fleet day. The maximum
  stays under 50%.
- `ls <root>` shows no container without a live owner.

## Tasks

- [ ] T1 — on-disk root (`worktree_root`/`root_from`), container with
  `owner.lock` and `wt/`, `--force --force` remove, root passed through the
  hook entrypoints; red→green tests for a successful and a failed run
- [ ] T2 — `sweep` (flock liveness, creation grace) and `prune_once`; the
  SIGKILL-then-next-run test and `sweep` unit tests
- [ ] T3 — `src/interrupt.rs` watcher, child registry, spawn helpers,
  `Interrupted` and re-raise; SIGTERM and SIGINT tests
- [ ] T4 — README, `gate_cache` doc, changelog, `0.4.0`; the fleet-day `/tmp`
  acceptance check

## Open Questions

Each one is designed against the stated assumption. None blocks T1.

1. **Prune side effect.** `git worktree prune` also drops the admin entry of a
   *user's* unlocked worktree whose checkout is missing (say, on an unmounted
   disk). Git's guard for this is `git worktree lock`.
   - Assumed: accept it, and document it in the README.
   - Alternative: prune only when `git worktree list --porcelain` shows a
     prunable `jj-hooks-worktree-` entry.
2. **Deeper paths under `$HOME`.** The checkout path grows from about 29 to
   about 70 characters. That is closer to the 108-byte unix-socket path limit
   for tools that bind sockets in the worktree. Parent-directory lookups
   (`~/node_modules`, `~/.npmrc`) also now reach `$HOME`.
   - Assumed: acceptable, with `JJ_HOOKS_WORKTREE_ROOT` as the escape hatch.
   - Alternative: a shorter default, such as `~/.cache/jj-hp/wt`.
3. **Version.**
   - Assumed: `0.4.0`, for the `Worktree::create` break.
   - Alternative: `0.3.13`, if the lib API counts as internal.
4. **SIGHUP under `nohup`.** A handler replaces an inherited `SIG_IGN`, and
   std plus `signal-hook` cannot read the old disposition without `unsafe`.
   Under `nohup`, a hangup would then abort the run instead of being ignored.
   - Assumed: accept it. Pane and terminal close is a leak source on the fleet.
   - Alternative: watch only SIGINT and SIGTERM.
