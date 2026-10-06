# Changelog

All notable changes to jj-hooks are tracked here.

## [Unreleased]

### Fixed

- Ctrl-C, SIGTERM or SIGHUP during a hook run now removes the hook worktree
  before `jj-hp` exits. The signal is forwarded to the hook as SIGTERM; a
  second one sends SIGKILL, and a third exits at once. After an interrupt
  `jj-hp` makes no fixup commit, advances no bookmark and does not push, and it
  still dies by the signal it received.

### Changed

- Off Linux, an inherited ignored SIGINT or SIGTERM (for example a `&` job in a
  script) becomes catchable. On Linux, an inherited ignore stays in force.

## [0.4.0]

Hook worktrees no longer leak onto tmpfs.

### Changed

- Hook worktrees live under an on-disk root, `~/.cache/jj-hooks/worktrees` by
  default, instead of `/tmp`. Override it with `JJ_HOOKS_WORKTREE_ROOT` or jj
  config `jj-hooks.worktree-root`. See "Hook worktrees" in the README.
- **Breaking (library):** `Worktree::create` takes the root as its first
  argument. `worktree_root` and `WORKTREE_PREFIX` are new public items.

### Fixed

- A `jj-hp` run killed mid-hook (Ctrl-C, SIGKILL, crash) no longer leaks its
  worktree for good. Each worktree carries a locked `<name>.lock` owner file,
  and the next run reaps every worktree whose lock is free and whose owner
  record checks out. Entries without an owner record get a one-hour grace.
- Removal uses `git worktree remove --force --force`, and a failed `git worktree
  add` cleans up its partial checkout.

### Upgrading

Worktrees leaked to `/tmp` by earlier versions have no owner lock, so the sweep
skips them. Delete the `/tmp/jj-hooks-worktree-*` directories, then run
`git worktree prune` in each affected repo.

## [0.3.12]

Distribution-only release: re-anchors the crates.io `repository`/binstall metadata
at the standalone repository and ships the first release through the consolidated
`mattwilkinsonn/tap` Homebrew tap; no functional change.

- Re-established as a standalone repository, extracted from the
  `mattwilkinsonn/zireael` monorepo at v0.3.11. Toolchain moved to devenv + its
  built-in `tasks` runner (dropping moon/proto), Rust pinned via rust-overlay, and
  shared dev tooling consumed from `mattwilkinsonn/dev-shared`.

## [0.3.11]

Baseline: the jj-hooks state at monorepo extraction. Full pre-extraction history
lives in the zireael monorepo.
