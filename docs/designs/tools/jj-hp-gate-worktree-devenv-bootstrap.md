# Design: jj-hp gate worktree — devenv bootstrap pre-warm

Status: **proposed**
Domain: tools

Tracking issue: `mattwilkinsonn/jj-hooks#13`. Crate: `jj-hooks` (this repo, the
standalone `jj-hp` binary), released, currently v0.3.11. This record and its
empirical findings sibling
(`docs/designs/tools/jj-hp-gate-worktree-devenv-bootstrap-findings.md`) were
carried over when jj-hooks was extracted from the `mattwilkinsonn/zireael`
monorepo back to standalone; the two sibling records it composes with — the
gate-cache + auto-allow record (monorepo #294) and the v0.3.9 repo-env
mechanism (monorepo #289) — remain in the zireael monorepo history and were not
carried.

## Problem / Intent

The jj-hp pre-push gate runs `moon ci` (via hk) inside an ephemeral detached
worktree under `/tmp/jj-hooks-worktree-*`. When a consumer repo's gate fans out
multiple parallel `devenv` evaluations (orion's `ci:build-image-*-amd64` legs),
the legs intermittently fail with "Failed to evaluate devenv configuration" at
`<worktree>/.devenv/bootstrap/default.nix` — worktree-only, parallel-only,
cold-store-only. Root cause (empirically established, full writeup in the
findings sibling `jj-hp-gate-worktree-devenv-bootstrap-findings.md`): devenv
self-regenerates `.devenv/bootstrap/` on eval, does NOT rewrite it when
present, and N parallel legs cold-regenerate the SAME
`<worktree>/.devenv/bootstrap/default.nix` concurrently on a cold store — the
loser reads transient state and the eval fails. A normal checkout has
bootstrap pre-materialized, so no leg regenerates and no race exists. The fix:
jj-hp pre-materializes `.devenv/bootstrap` in the gate worktree BEFORE any
hook runner fires, so the worktree behaves like a normal checkout.

Matt ruled Option C (2026-08-23, frozen): jj-hooks pre-warms bootstrap
universally (this record); the orion lane separately removes its ineffective
`DEVENV_DOTFILE` mitigation (out of scope here). Hard constraint from the same
ruling: "jj-hp isn't always used with devenv" — the pre-warm MUST detect
devenv-in-use and cleanly no-op otherwise.

## Global Constraints

- **The frozen v0.3.9 non-regression contract holds.** A repo with no devenv
  (no `devenv.nix`/`devenv.yaml`), with direnv-but-not-devenv, with no `.envrc`
  at all, or with the pre-warm opted out MUST be byte-identical to today:
  subprocess env AND worktree filesystem unchanged — no `.devenv/` created, no
  extra subprocess, no extra stat beyond the detection probe. Pre-warm failure
  is never fatal: warn once (tracing) and fall back to today's behavior. The
  mechanism helps the devenv case and never regresses it: the one window where
  a copy could be worse than the status-quo (no-`.devenv/`) gate — a stale
  primary bootstrap after a devenv-binary upgrade without a primary re-eval —
  is closed by a **freshness guard** (Matt's ruling, 2026-08-23): the copy is
  skipped in favor of status-quo regen when the primary bootstrap is older than
  `devenv.lock`. So a copy is only ever made when it is at least as fresh as
  what the worktree would regenerate.
- **The unconditional git repo-location strip stays load-bearing and
  untouched.** `apply_repo_env`/`strip_git_local_env`
  (`repo_env.rs:105-160`) run on every spawn regardless of patch state. The
  pre-warm is a filesystem operation on the worktree — it touches no `Command`
  env and composes with the strip by construction.
- **The fixup tree must stay clean.** `maybe_build_fixup_commit` runs
  `git add -A` in the worktree and compares tree OIDs (`hooks.rs:1204-1213`).
  The copied `.devenv/bootstrap` must never enter that tree: devenv repos
  gitignore `.devenv*` (this repo: `.gitignore:18`), and the pre-warm guards
  the exotic non-ignored case explicitly (see Approach, fidelity guards).
- **No new runtime dependency.** The copy is `std::fs`; detection is file
  stats. devenv itself is never invoked by the pre-warm (it is already
  required — by the repo — for the devenv case, but jj-hp cannot assume it on
  its own PATH). Rust toolchain 1.96.0 (`rust-toolchain.toml`).
- **Red→green tests on the existing harness**: tempdir remote+primary fixture
  (`tests/harness/mod.rs:16-29`), batch API (`tests/parallel_batch.rs:9-20`),
  CLI pipeline (`tests/push.rs`), real-direnv nextest-only fixture
  (`tests/repo_env_real.rs:38-72`). At least one test asserts a gate worktree
  for a devenv repo has `.devenv/bootstrap` materialized BEFORE any hook
  runner fires, and at least one asserts a NON-devenv repo is byte-identical
  (no `.devenv/` created, no extra work).
- **Commit identity** per `rule://commit-conventions` (author mintaka + the
  `Co-authored-by: Matt Wilkinson <mattwilki17@gmail.com>` trailer). This
  record ships FIRST as a docs-only PR.
- **Single-crate version number** — bump the one `[package].version` field
  (`Cargo.toml:4`, currently `0.3.11`). The standalone is a single crate, not a
  workspace: there is no `[workspace.package]` field and no internal path-dep
  pin to rewrite (the monorepo's jj-gt→jj-hooks path dep is now a crates.io
  version dep in the separate jj-gt repo). **Markdownlint-clean** under
  `.markdownlint-cli2.jsonc` (MD013 off).

## Evidence base

All file:line references are to this standalone repo's `src/`/`tests/`/root
(the `src/**` tree is a byte-identical re-home of the monorepo's
`tools/jj-hooks/`, so the line numbers carry over unchanged). The empirical
devenv findings are in the findings sibling
(`docs/designs/tools/jj-hp-gate-worktree-devenv-bootstrap-findings.md`) and are
cited as established.

- **Worktree creation — the injection seam** (`worktree.rs:40-73`,
  `Worktree::create`):

  ```rust
  pub fn create(git_dir: &Path, commit: &str) -> Result<Self> {
      let dir = TempDir::with_prefix("jj-hooks-worktree-")?;
      // ...
      let output = Command::new("git")
          .arg(format!("--git-dir={}", git_dir.display()))
          .args(["worktree", "add", "--detach", "--quiet"])
  ```

  Creation is serialized by `WORKTREE_CREATE_LOCK` (`worktree.rs:30`) but has
  no devenv/workspace context — `create(git_dir, commit)` receives neither
  `workspace_root` nor any repo-env state. This is why the pre-warm does NOT
  live inside `Worktree::create` (fork 3).

- **The one worktree-creation callsite** (`hooks.rs:784`, inside `run_once`):

  ```rust
  let wt = Worktree::create(primary_git_dir, target_commit)?;
  ```

  immediately followed by the setup steps
  (`setup::run_steps(setup_steps, wt.path(), workspace_root)`,
  `hooks.rs:805`) and then the hook runner fan-out (`run_subprocess` calls at
  `hooks.rs:950`/`:978`). `run_once` has `workspace_root` in scope — the
  natural slot for a once-per-worktree, devenv-aware step is between `:784`
  and `:805`.

- **`.devenv` is gitignored, so the detached worktree never has it**
  (`.gitignore:16-18`):

  ```text
  # devenv + direnv: generated shell artifacts + direnv cache. The devenv.{nix,yaml},
  # devenv.lock, and .envrc sources are committed.
  .devenv*
  ```

  The committed devenv sources are `devenv.nix`, `devenv.yaml`, `devenv.lock`,
  `.envrc` (all present at this repo's root). So a
  fresh `git worktree add --detach` materializes the devenv SOURCES but not
  the generated `.devenv/` — bootstrap regen is structural.

- **The bootstrap files are small, path-portable, and version-derived**
  (from this repo's live `.devenv/bootstrap/`):
  `default.nix` (307 B), `bootstrapLib.nix` (21,833 B), `resolve-lock.nix`
  (5,476 B). `default.nix` in full:

  ```nix
  args@{ system
  , # The project root (location of devenv.nix)
    devenv_root
  , ...
  }:

  let
    inherit
      (import ./resolve-lock.nix {
        src = devenv_root;
        inherit system;
      })
      inputs
      ;

    bootstrapLib = import ./bootstrapLib.nix { inherit inputs; };
  in

  bootstrapLib.mkDevenvForSystem args
  ```

  Relative imports only; the root comes in as the `devenv_root` argument and
  the lock is read from `src` — nothing absolute is baked in, so a copy into a
  different root is exact. (Established empirically: same devenv binary →
  same bootstrap content; devenv does not rewrite bootstrap when present,
  md5+mtime stable across evals.)

- **`run_once`'s remaining pipeline — where "before any hook runner" is
  measured** (`hooks.rs:899-983`): the hk Pkl warm (`:899-908`), then the
  all-files or per-from-ref `run_subprocess` loop (`:940-983`). The setup
  steps at `:805` also spawn user commands in the worktree — a consumer's
  setup step could itself invoke devenv, so the pre-warm must land BEFORE
  `run_steps`, not merely before the runner.

- **The fixup tree comparison the pre-warm must not pollute**
  (`hooks.rs:1204-1213`):

  ```rust
  run_git(worktree, &["add", "-A"])?;
  let tree = run_git_capture(worktree, &["write-tree"])?;
  // ...
  if tree == parent_tree {
      return Ok(None);
  }
  ```

  `git add -A` stages untracked files, but gitignored paths are excluded —
  `.devenv*` is ignored in every devenv repo that follows devenv's own
  scaffolding (this repo: `.gitignore:18`). The detection fork (fork 1)
  additionally requires the primary's generated bootstrap to exist, which
  only happens for repos where devenv manages `.devenv/` — but a repo that
  tracked `.devenv` in git would break this assumption, so T1 guards it (skip
  the pre-warm when `.devenv` is not ignored in the worktree, checked with
  `git check-ignore`).

- **The opt-out pattern to mirror** (`repo_env.rs:162-231`):
  `repo_env_enabled` (`:170-174`) — env `JJ_HOOKS_NO_REPO_ENV`, config
  `jj-hooks.repo-env`; `repo_env_autoallow_enabled` (`:197-203`) — env
  `JJ_HOOKS_NO_DIRENV_ALLOW`, config `jj-hooks.repo-env-autoallow`, with the
  strict-parse pure core `autoallow_from` (`:210-231`):

  ```rust
  Some(c) => {
      tracing::warn!(
          "jj-hp: unrecognized value {c:?} for jj config \
           `jj-hooks.repo-env-autoallow` (expected `true` or `false`); \
           auto `direnv allow` stays enabled"
      );
  ```

  And the newer twin `gate_cache.rs` (from #294/T1): opt-out read once at the
  entrypoints (`gate_cache_enabled`, `gate_cache.rs:95-99`), decision cached
  process-globally keyed by canonicalized `workspace_root`
  (`gate_cache.rs:33-47`), applied by a low-level helper with no `JjCli` in
  scope (`apply_gate_cache`, `gate_cache.rs:61-84`), plus a `#[cfg(test)]
  test_seed` (`gate_cache.rs:130-139`). The pre-warm reuses this exact shape.

- **The three eager entrypoints where opt-outs are read once**
  (`hooks.rs:160-167` in `run_for_update`, `hooks.rs:446-451` in
  `run_for_updates_parallel`, `hooks.rs:574-579` in
  `run_for_partitioned_updates_parallel`; the CLI/revset path reaches
  `run_for_update` via `lib.rs:463-471`):

  ```rust
  let _ = crate::repo_env::repo_env(
      workspace_root,
      crate::repo_env::repo_env_enabled(jj),
      crate::repo_env::repo_env_autoallow_enabled(jj),
  );
  crate::gate_cache::gate_cache(workspace_root, crate::gate_cache::gate_cache_enabled(jj));
  ```

  The pre-warm's opt-out read slots beside these lines at all three
  entrypoints; the pre-warm itself runs later, per-worktree, in `run_once`.

- **`EnvPatch` carries no devenv discriminator** (`repo_env.rs:32-40`):
  `Disabled` / `Patch(HashMap<String, Option<String>>)`. A `Patch` is produced
  for ANY allowed `.envrc` (the real-direnv fixture uses a plain
  `export FOO=bar` `.envrc`, `tests/repo_env_real.rs:82`), so "patch is
  non-empty" or even "patch has `DEVENV_*` keys" is direnv-layer evidence, not
  a devenv-in-use source of truth — and it is unavailable when repo-env is
  opted out while the devenv race still exists. Hence fork 1 uses filesystem
  detection, not the patch.

- **Test-harness grounding**: `TestRepo` tempdir remote+primary fixture
  (`tests/harness/mod.rs:16-29`); recording-hook precedent for observing the
  child/worktree state (`PRE_PUSH_RECORD_CARGO_TARGET_DIR`,
  `tests/harness/mod.rs:677-688`, and the per-bookmark batch variant at
  `:696-707`); 3-bookmark batch builder (`tests/parallel_batch.rs:25-50`);
  real-direnv isolation + nextest-only requirement
  (`tests/repo_env_real.rs:14-20`, `:38-54`).

## Approach

One small mechanism: a new `bootstrap_prewarm` module (sibling of
`gate_cache.rs`, mirroring its shape exactly) that, once per worktree and only
when devenv is in use, copies the PRIMARY workspace's generated
`.devenv/bootstrap/` into the fresh worktree before any subprocess runs in it.
The first devenv eval in the worktree then finds bootstrap present, devenv
does not regenerate it (established: not rewritten when present), no leg
races, and the gate behaves like a normal checkout. Everything else — env
propagation, git-strip, gate-cache — is untouched.

### Fork 1 — detection: devenv sources at the root + a usable primary bootstrap

**Chosen:** devenv is "in use" iff `workspace_root` has `devenv.nix` OR
`devenv.yaml` (the committed devenv sources — `.gitignore:16-17` names them as
"committed" by convention, and both exist at this repo's root). Whether the
pre-warm can then actually MATERIALIZE is a second, independent check:
`<workspace_root>/.devenv/bootstrap/default.nix` exists (the primary has
evaluated at least once). Two distinct outcomes:

- devenv sources absent → NOT a devenv repo → strict no-op (the frozen
  contract path). No log line — this is the steady state for every non-devenv
  consumer.
- devenv sources present but primary bootstrap absent → devenv repo, nothing
  to copy → skip with a single `tracing::info!` (see fork 2 for why skip, not
  warm).

**Why not the alternatives:**

- *`EnvPatch::Patch` carrying `DEVENV_*` keys*: direnv-layer evidence, not
  devenv-layer. It is (a) absent when repo-env is opted out
  (`JJ_HOOKS_NO_REPO_ENV`) even though the devenv race still exists and the
  gate still runs devenv via moon; (b) empty-map on a shell that already has
  the repo env loaded (`repo_env.rs:36-38`: "An EMPTY map is a success");
  (c) coupling two independent features — the brief's layering note (direnv
  without devenv exists) cuts both ways: devenv detection should not ride the
  direnv patch.
- *Primary `.devenv/bootstrap` presence ALONE*: conflates "devenv repo" with
  "primary has evaluated". A devenv repo whose primary was never evaluated
  would be indistinguishable from a non-devenv repo, and the info-level skip
  signal (useful for diagnosing a still-racing gate) would be impossible.
  Keeping the two probes separate also gives the non-devenv path exactly two
  `stat` calls and zero log noise.
- *`.envrc` contents (grep for `use devenv`)*: parsing shell is fragile
  (indirection via `source_env_if_exists`, custom wrappers), and `devenv.nix`/
  `devenv.yaml` is the thing devenv itself requires to exist.

**Bound on "universal":** detection keys on `devenv.{nix,yaml}` AT
`workspace_root`. A monorepo whose `devenv_root` is a SUBDIRECTORY (devenv
sources not at the repo root) is not detected and keeps the status quo. This is
a false-negative, never a false-positive — a miss = today's cold-regen
behavior, never worse — so it's non-load-bearing (the only known racing
consumer, orion, is root-level). Noted so "universal" is understood as
"every root-level devenv consumer," not literally every layout.

### Fork 2 — materialization: `std::fs` copy; skip when the primary has none

**Chosen: (a) copy, materialized atomically.** Recursively copy the contents
of `<workspace_root>/.devenv/bootstrap/` into a temp staging dir on the SAME
filesystem inside the worktree (`<worktree>/.devenv/.bootstrap.tmp-<unique>`),
then `fs::rename` it atomically onto `<worktree>/.devenv/bootstrap` — so the
worktree only ever observes bootstrap as absent-or-complete, never partial
(a recursive walk — `create_dir_all` per directory level + `fs::copy` per file,
NOT a single flat `readdir`+`fs::copy` pass, so a future NESTED bootstrap
subdir is materialized rather than dropped; in this repo
that is three files, ~27 KB total — the copy walks the
directory rather than hardcoding names, so a devenv version that adds a
bootstrap file needs no jj-hp change). Rationale:

- **Byte-identical to a real checkout (the correctness argument).** The copy
  makes the worktree's `.devenv/bootstrap` byte-identical to the primary
  checkout's, and the primary demonstrably evaluates with that exact bootstrap
  under the repo-pinned devenv (that is how the user's own shell/CI works). The
  worktree is a fresh checkout of the same tree at the same devenv pin, so the
  copied bootstrap is exactly the one it would itself regenerate — the copy
  only removes the *concurrent* regen, it never introduces a bootstrap the
  worktree wouldn't have produced. (Note: because devenv does not rewrite
  bootstrap when present — established — a copied bootstrap is *used as-is*, not
  re-derived; correctness therefore rests on this byte-identity-to-primary
  argument, NOT on any regen-on-mismatch fallback, which presence suppresses.
  Staleness across a devenv upgrade is handled in Open Questions.)
- **Atomic, so a failed copy is never worse than status quo.** Staging +
  `fs::rename` means a partway-failed copy leaves only the temp dir (best-effort
  removed), never a corrupt-but-present `bootstrap/` at the real path. This is
  load-bearing: without it, a copy that fails after `default.nix` lands but
  before `bootstrapLib.nix` — with the partial-cleanup ALSO failing for the
  same I/O reason (perms, ENOSPC) — would leave a corrupt bootstrap that devenv
  trusts (not-rewritten-when-present) and every leg fails *deterministically*,
  strictly worse than today's *intermittent* race. The atomic rename removes
  this partial-corruption failure mode entirely (the remaining stale-copy
  regression is a separate, bounded case — see the Open Question).
- **No hand-symlink.** The AGENTS.md house rule bars hand-created symlinks
  (nix `mkOutOfStoreSymlink` being the only exception, which a jj-hp-created
  symlink is not). A symlink would also let a gate-triggered regen WRITE
  THROUGH into the primary's `.devenv/bootstrap` — mutating the user's
  primary state from an ephemeral gate is exactly the class of hazard the
  git-strip invariant exists to prevent. Copy is strictly safer and the
  bytes are trivial.
- **No warming eval.** Invoking `devenv` from jj-hp would add a runtime
  dependency on the devenv binary being resolvable from jj-hp's own PATH
  (it often is not — the pinned devenv usually lives in the direnv-loaded
  env), pay a potentially multi-second serialized eval on every gate, and
  re-introduce output-capture concerns. Rejected as the primary mechanism.

**Primary-bootstrap-absent case: skip, do not warm.** When the primary has
never evaluated (`.devenv/bootstrap/default.nix` missing), the pre-warm logs
one `tracing::info!` and does nothing. The first gate on such a clone keeps
today's behavior — the parallel legs cold-regenerate — which is exactly the
status quo, never worse (frozen contract). A warming eval here would import
all the rejected costs above for a one-time edge (a clone whose OWNER has
never once entered the devenv shell — rare, since direnv auto-evaluates on
first entry and OMP auto-allows). Accepted residual, noted in Open Questions
as a non-load-bearing deferral.

**Fidelity guards (all cheap, all in T1):**

- Materialize ONLY if `<worktree>/.devenv` does not already exist (it never
  does today — the path is gitignored — but a future tracked `.devenv` must
  win).
- Before materializing, `git check-ignore .devenv` in the worktree; proceed
  ONLY on exit 0 (ignored). Treat exit 1 (not ignored) AND exit 128 (git
  error) as warn-skip — an unignored copy would enter the fixup tree
  (`hooks.rs:1204-1213`, `git add -A` stages untracked-but-not-ignored paths)
  and corrupt the content-addressed fixup gate, and an errored check can't
  prove it's ignored.
- **Freshness guard (Matt's ruling):** before materializing, compare the mtime
  of `<workspace_root>/.devenv/bootstrap/default.nix` against
  `<workspace_root>/devenv.lock`. If the bootstrap is OLDER than the lock, skip
  the copy (`tracing::info!`) and let the worktree regenerate fresh — exactly
  the status-quo path. This closes the devenv-binary-upgraded-without-reeval
  window: a stale primary bootstrap is never propagated into the gate. `mtime`
  is the cheap, dependency-free signal (`devenv.lock` is rewritten on every
  `devenv update`; the bootstrap is regenerated on the next eval after an
  upgrade), and a missing/unreadable mtime on either side degrades to skip
  (never-fatal, status quo). The common case (bootstrap newer than the lock)
  copies as before.
- Any I/O error (permission, ENOSPC, TOCTOU disappearance of a source file):
  warn once, best-effort remove the temp staging dir (never the final path,
  which the atomic rename guarantees is untouched on failure), continue —
  never fatal, per the frozen contract.

### Fork 3 — placement: dedicated call in `run_once`, right after worktree create

**Chosen:** one call at the top of the worktree's life, in `run_once` between
`hooks.rs:784` (`let wt = Worktree::create(...)`) and `hooks.rs:805`
(`setup::run_steps(...)`):

- **Once per worktree**: `run_once` creates exactly one worktree per pass
  (`hooks.rs:784` is the sole `Worktree::create` callsite — grepped this
  session); every retry pass builds a fresh worktree and gets a fresh
  pre-warm.
- **Before ANY subprocess in the worktree**: setup steps (`:805`) run user
  commands that may themselves invoke devenv; the hk Pkl warm (`:899-908`)
  and the runner fan-out (`:940-983`) all come later. Landing before
  `run_steps` covers all of them.
- **`workspace_root` is in scope** (`run_once` parameter, `hooks.rs:764`), so
  the detection probes and the copy source need no plumbing.

**Why not the alternatives:**

- *Inside `Worktree::create`*: it has no `workspace_root` (signature
  `create(git_dir: &Path, commit: &str)`, `worktree.rs:40`) and no opt-out
  state; threading both through a git-plumbing constructor couples the
  worktree abstraction to a devenv feature. Also `WORKTREE_CREATE_LOCK` is
  deliberately narrow ("The lock only covers the `git worktree add` call
  itself", `worktree.rs:24-25`) — a copy inside it would widen the serialized
  section for every consumer, devenv or not.
- *A synthetic `[[jj-hooks.setup]]` step*: setup steps are user-declared
  config (`setup.rs:91-104` loads them from jj config); injecting a synthetic
  one would leak into user-visible output labeling (`--- setup step ... ---`,
  `setup.rs:166`) and make opt-out semantics weird. The pre-warm is
  jj-hp-internal provisioning, not a user step.

The opt-out + enablement decision is read ONCE per process at the three eager
entrypoints (beside the existing `repo_env`/`gate_cache` reads,
`hooks.rs:160-167`/`:446-451`/`:574-579`) and cached process-globally keyed by
canonicalized `workspace_root` — the exact `gate_cache.rs:33-59` shape — so
`run_once` (no `JjCli` needed) just calls the apply-side helper. A
process-global once-per-worktree-path guard is unnecessary: `run_once` runs
the pre-warm exactly once per worktree by construction, and worktree tempdirs
are unique (`TempDir::with_prefix`, `worktree.rs:41`).

### Fork 4 — opt-out: its own axis (`JJ_HOOKS_NO_BOOTSTRAP_PREWARM`)

**Chosen:** a separate switch, mirroring the `gate_cache_enabled` strict-parse
pattern (`gate_cache.rs:86-125`, itself mirroring `repo_env.rs:162-231`):

1. env `JJ_HOOKS_NO_BOOTSTRAP_PREWARM` — any non-empty value disables;
2. jj config `jj-hooks.bootstrap-prewarm` — `"off"` disables (case- and
   space-insensitive); unset or `"auto"` enables; any other non-empty value
   warns (tracing) and enables.

**Why not riding `jj-hooks.repo-env`:** the pre-warm is NOT part of the
env-propagation feature — it fixes a filesystem race that exists even when
repo-env is opted out (the gate still runs `moon ci`, which still fans out
devenv evals). Coupling them would make `JJ_HOOKS_NO_REPO_ENV` silently
re-expose the race, and would give users no way to keep the env feature while
disabling the copy (e.g. to debug a suspected stale-bootstrap issue). Every
prior mechanism in this crate got its own axis for the same reason
(`repo-env`, `repo-env-autoallow`, `gate-cache`); a fourth axis is the
established convention, not new surface.

### Landing order

T1 (mechanism + opt-out + tests) is the fix; T2 closes docs + release. The
orion-lane `DEVENV_DOTFILE` removal is out of scope (Matt's Option C ruling
splits it to that lane).

## Plan

First release ships T1 + T2.

### T1 — `bootstrap_prewarm` module + wiring + opt-out

New module `src/bootstrap_prewarm.rs`, shaped exactly like
`gate_cache.rs` (process-global enabled-cache keyed by canonicalized
`workspace_root`, populated at the entrypoints, read by a low-level apply
helper; `#[cfg(test)] test_seed`).

Interfaces:

```rust
/// Record whether the bootstrap pre-warm is enabled for `workspace_root`.
/// Called EAGERLY once at each batch entrypoint, beside the existing
/// repo_env/gate_cache reads (hooks.rs:160-167, :446-451, :574-579).
pub fn bootstrap_prewarm(workspace_root: &Path, enabled: bool);

/// Read the opt-outs (mirrors gate_cache_enabled, gate_cache.rs:95-99):
/// 1. JJ_HOOKS_NO_BOOTSTRAP_PREWARM env var — any non-empty value disables.
/// 2. jj config `jj-hooks.bootstrap-prewarm` — "off" disables; unset/"auto"
///    enables; any other non-empty value warns (tracing) and enables.
pub fn bootstrap_prewarm_enabled(jj: &crate::jj::JjCli) -> bool;

/// Pure decision core, testable without a live jj repo (mirrors
/// gate_cache.rs enabled_from, :105-125).
fn enabled_from(env_optout: Option<&std::ffi::OsStr>, config: Option<&str>) -> bool;

/// Pre-materialize `.devenv/bootstrap` into a fresh gate worktree. Called
/// ONCE per worktree from run_once, between Worktree::create (hooks.rs:784)
/// and setup::run_steps (hooks.rs:805). Never fatal, never returns an error:
/// every skip/failure path warns or infos via tracing and returns.
///
/// Sequence:
/// 1. enabled-cache says disabled/unpopulated → strict no-op.
/// 2. neither `devenv.nix` nor `devenv.yaml` at workspace_root → no-op
///    (not a devenv repo; no log).
/// 3. `<workspace_root>/.devenv/bootstrap/default.nix` missing → info-skip
///    (devenv repo, primary never evaluated; first gate keeps today's
///    cold-regen behavior).
/// 4. Freshness guard (Matt's ruling): primary `bootstrap/default.nix` mtime
///    OLDER than `<workspace_root>/devenv.lock` → info-skip (stale copy would
///    regress vs status-quo regen; let the worktree regenerate fresh). A
///    missing/unreadable mtime on either side → skip (never-fatal).
/// 5. `<worktree>/.devenv` already exists → no-op (a tracked .devenv wins).
/// 6. `git check-ignore .devenv` in the worktree is NOT ignored (exit != 0)
///    → warn-skip (an unignored copy would enter the fixup tree,
///    hooks.rs:1204-1213).
/// 7. Recursively copy `<workspace_root>/.devenv/bootstrap/*` into a temp
///    staging dir, then `fs::rename` atomically onto
///    `<worktree>/.devenv/bootstrap/`. On any I/O error: warn once,
///    best-effort remove the temp staging dir (never the final path), return.
pub fn apply_bootstrap_prewarm(worktree: &Path, workspace_root: &Path);
```

Consumes: `workspace_root` + `wt.path()` (both in scope in `run_once`,
`hooks.rs:764`/`:784`); `JjCli` only at the three entrypoints for the opt-out
read. Wiring: one `bootstrap_prewarm(...)`+`bootstrap_prewarm_enabled(jj)`
line at each entrypoint beside the `gate_cache` call; one
`apply_bootstrap_prewarm(wt.path(), workspace_root)` call in `run_once`
immediately after `hooks.rs:784`.

Produces: a devenv repo's gate worktree has `.devenv/bootstrap` materialized
before any setup step, Pkl warm, or hook runner spawns; non-devenv repos get
two stats and nothing else.

Test cycle (red→green, existing harness):

- Unit (`bootstrap_prewarm.rs` tests): `enabled_from` precedence — env beats
  config; `"off"` disables; unset/`"auto"` enables; unrecognized warns and
  enables (WARN_COUNT pattern, `gate_cache.rs:127-128`).
- Unit (tempdirs, no jj needed): `apply_bootstrap_prewarm` — devenv sources +
  primary bootstrap present + bootstrap newer than `devenv.lock` → files copied
  byte-identical; devenv sources absent → worktree untouched (no `.devenv`
  created); sources present but primary bootstrap absent → untouched; primary
  bootstrap OLDER than `devenv.lock` (freshness guard) → skip, worktree
  untouched (status-quo regen path); pre-existing `<worktree>/.devenv` →
  untouched; failed materialization → the ATOMICITY invariant: no
  `.devenv/bootstrap` is ever visible at the final path (only the temp staging
  dir, best-effort removed), so the worktree is never left with a
  corrupt-but-present bootstrap. Force the failure DETERMINISTICALLY (no
  retries, per `rule://no-retries`) with a chmod-0 source file, so a per-file
  `fs::copy` errors mid-walk INTO the temp staging dir — the final `bootstrap/`
  path is never populated, and the test asserts it stays absent. (A
  rename-failure seam must NOT pre-create `<worktree>/.devenv` itself: that
  trips the existence no-op (sequence step 5) before staging, so the rename
  never runs and the test passes vacuously — the chmod-0-source seam covers the
  invariant without colliding with any guard.)
- Integration (`TestRepo`, `tests/harness/mod.rs:16-29`): commit a fake
  `devenv.nix` + a `.gitignore` with `.devenv*`, seed a fake
  `<primary>/.devenv/bootstrap/default.nix`; pre-push hook records whether
  `$PWD/.devenv/bootstrap/default.nix` exists (the
  `PRE_PUSH_RECORD_CARGO_TARGET_DIR` recording-hook pattern,
  `tests/harness/mod.rs:677-688`); run the pipeline; assert the hook saw it
  (RED before T1: absent). Twin assertion through a setup step, proving the
  before-run_steps ordering.
- Non-regression (frozen contract): same pipeline WITHOUT `devenv.nix` →
  the recording hook reports no `.devenv` in the worktree; and with
  `JJ_HOOKS_NO_BOOTSTRAP_PREWARM=1` on a devenv-shaped repo → likewise
  absent (byte-identical opt-out).
- Fixup-purity guard: devenv-shaped repo WITHOUT `.devenv*` in `.gitignore`
  → pre-warm skips (warn path) and no fixup commit is produced for an
  otherwise clean pass.
- Batch (`tests/parallel_batch.rs:25-50` 3-bookmark builder): per-bookmark
  recording hook (the `:696-707` per-child-file pattern) asserts every
  parallel worktree got its own materialized bootstrap.

Acceptance: driver-run on orion — a gate run of the `ci:build-image-*-amd64`
fan-out in a jj-hp worktree finds `.devenv/bootstrap` pre-materialized (no
leg regenerates it: md5+mtime of `bootstrap/default.nix` stable across the
run), and repeated gate runs no longer reproduce "Failed to evaluate devenv
configuration". (A forced cold-store repro remains impractical — see the
findings' honesty caveat; stability-of-bootstrap is the observable proxy.)

### T2 — docs + release

Update `README.md`: the pre-warm mechanism (what it copies,
when it no-ops, the primary-never-evaluated skip) and the
`JJ_HOOKS_NO_BOOTSTRAP_PREWARM` / `jj-hooks.bootstrap-prewarm` opt-out.
Changelog entry referencing #13. Version bump: the single `[package].version`
field (`Cargo.toml:4`, currently `0.3.11`); no public API change (one new
module, no exported-type change), so `0.3.12` is the natural bump.

Interfaces: none new — docs + the one version field.

Test cycle: `cargo fmt -p jj-hooks -- --check`, `cargo clippy -p jj-hooks
--all-targets -- -D warnings`, `cargo nextest run -p jj-hooks`, markdownlint
on touched docs — all green (`rule://pre-finish-checks`). Driver-run
acceptance: the T1 orion scenario re-verified on the released binary, and the
orion lane unblocks RIG-2569 without `--no-hooks`.

## Tasks

- [ ] T1 — `bootstrap_prewarm.rs`: detection (`devenv.nix`/`devenv.yaml` +
  primary `.devenv/bootstrap/default.nix`), the freshness guard (skip when
  primary bootstrap older than `devenv.lock`), recursive atomic `std::fs`
  copy (temp staging + `fs::rename`) into the worktree with the check-ignore +
  pre-existing-`.devenv` + never-fatal guards, wired into `run_once` after
  `Worktree::create` and gated by `JJ_HOOKS_NO_BOOTSTRAP_PREWARM` /
  `jj-hooks.bootstrap-prewarm` (strict parse, read once at the three
  entrypoints); unit + integration + non-regression + fixup-purity + batch
  tests; driver acceptance on orion
- [ ] T2 — README (mechanism + opt-out) + changelog (#13) + version bump to
  `0.3.12` (`Cargo.toml:4`); full fmt/clippy/nextest/markdownlint

## Resolved decisions

- **[stale-bootstrap regression → freshness guard]** (Matt ruled 2026-08-23.)
  The copy could regress vs the status-quo (no-`.devenv/`) gate in one bounded
  window — the owner upgraded the devenv BINARY without re-evaluating the
  primary, so the primary's bootstrap is stale relative to the gate's binary,
  and (devenv not rewriting bootstrap when present) the gate would be frozen on
  the stale copy where status-quo would regenerate fresh. **Decision: add the
  freshness guard** (option b) — skip the copy when the primary
  `bootstrap/default.nix` is older than `<workspace_root>/devenv.lock`, falling
  back to status-quo regen. Folded into Fork 2's fidelity guards, the T1
  sequence (step 4), and a T1 unit test. The window is closed: a copy is only
  ever made when at least as fresh as what the worktree would regenerate.

## Open Questions

One **non-load-bearing deferral** — the design is correct without it, recorded
with the designed-against assumption:

- **[primary-never-evaluated residual]** When a devenv repo's primary has no
  `.devenv/bootstrap` (owner never entered the devenv shell), the pre-warm
  skips and the first gate keeps today's cold-regen race exposure — exactly
  the status quo, never worse. Designed-against assumption: this population is
  effectively empty (direnv evaluates on first shell entry; OMP auto-allows;
  jj-hp's own auto-allow from #294 resolves blocked clones). If Matt wants
  that edge closed too, the follow-up would be a serialized warming
  `devenv shell -- true`-style eval behind the same opt-out — deliberately NOT
  in this record (adds a devenv-binary PATH dependency + a multi-second
  serialized eval to first gates).
