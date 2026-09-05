# jj-hp gate: devenv worktree build-image failure — empirical root cause

Empirical investigation (jj-hooks lane; carried over from the zireael monorepo
at extraction as the findings sibling of the bootstrap pre-warm design record,
`docs/designs/tools/jj-hp-gate-worktree-devenv-bootstrap.md`).
Peer `org/service-owner` reported: pre-push `hk` gate's `ci:build-image-*-amd64`
legs fail in the ephemeral `/tmp/jj-hooks-worktree-*` worktree with "Failed to
evaluate devenv configuration" at `.devenv/bootstrap/default.nix`; blocked an
internal tracker issue (RIG-2569); worked around with `jj-vine --no-hooks`.

## Peer's stated root cause — DISPROVEN

Claim: "the worktree does NOT get a `.devenv/` directory, so the bootstrap
import fails." Empirically false — **devenv self-regenerates `.devenv/bootstrap/`
on eval**:

- Bare `git worktree add --detach` off zireael, no `.devenv/`: `devenv info`
  regenerates the full `.devenv/` (bootstrap + eval-cache). Eval succeeds.
- Fresh `git archive HEAD | tar -x` of orion (tracked files only, no `.devenv/`):
  `devenv container build ci-base --system x86_64-linux --no-eval-cache` →
  **exit 0**, emits `image-orion-ci-base.json`. This is the peer's own
  "succeeds in a real checkout" control, reproduced in a bootstrap-LESS tree.
- Faithful `git worktree add --detach` off orion's `.git` (`.git` a file
  pointer, no `.devenv/`): same command → **exit 0**.
- Even injecting the PRIMARY's `DEVENV_ROOT`/`DEVENV_STATE`/`DEVENV_DOTFILE`
  (as jj-hp's `repo_env` does) into the bootstrap-less tree → **exit 0**.

So "worktree lacks `.devenv/`" is NOT the differentiator. The error naming
`.../bootstrap/default.nix` is an eval failure with bootstrap PRESENT, not a
missing-file import.

## True root cause — concurrent cold bootstrap regen race

- **devenv does NOT rewrite `bootstrap/default.nix` when present** (md5 + mtime
  stable across repeated evals). So a materialized bootstrap → no regen.
- **orion's existing race mitigation is INEFFECTIVE.** `publish-ci-image.ts:306-308`
  (`devenvDotfile`) sets a per-leg `DEVENV_DOTFILE=/tmp/orion-ci-devenv-<attr>-<system>`
  specifically to stop "two parallel legs race on `bootstrap/default.nix` and one
  fails its import eval." PROVEN not to work: bootstrap regenerates under
  `<repo>/.devenv/bootstrap` regardless of `DEVENV_DOTFILE` (a real
  `bun publish-ci-image.ts` leg left bootstrap in `$WT/.devenv/bootstrap`, the
  `/tmp` override dir never got one). So the intended isolation does not happen.
- **Mechanism:** `moon ci` (via hk) runs multiple affected `build-image-*-amd64`
  legs IN PARALLEL inside the ONE jj-hp worktree. On a COLD store (RIG-2569
  changed image inputs → wide eval windows), the legs cold-regenerate the SAME
  `<worktree>/.devenv/bootstrap/default.nix` concurrently; the loser reads a
  transient/partial state → "Failed to evaluate devenv configuration." A normal
  checkout has bootstrap pre-materialized → no regen → no race. Matches all
  three observed facts: worktree-only, parallel-legs-only ("only the 2
  build-image legs failed"), cold-store-triggered.

## Honesty caveat

Could NOT force the race live on a warm store — the regen window is sub-ms and
single-file writes appeared atomic in polling (0 partial/empty samples over
~40k samples; 0 incremental-population windows over 6 trials). The shared nix
store was not destroyed to force a cold-store repro. Root cause rests on: (1) the
disproof of the missing-dir theory, (2) proof that orion's DEVENV_DOTFILE
isolation is ineffective, (3) proof bootstrap is not rewritten when present, and
(4) exact symptom triangulation (worktree + parallel + cold). A cold-store live
repro would upgrade this from strongly-grounded to demonstrated.

## Fix candidates (the design fork, ruled by Matt → Option C)

- **A (jj-hooks lane):** pre-materialize/pre-warm `.devenv/bootstrap` in the
  worktree BEFORE hooks run (one serialized warming eval, or copy/symlink
  primary's bootstrap — bootstrap is devenv-version-derived, safe to share).
  First eval finds it present → no leg regenerates → no race. UNIVERSAL (fixes
  every jj-hp gate consumer), at the provisioning layer jj-hooks owns. Recommended.
- **B (orion, another lane):** fix its ineffective DEVENV_DOTFILE mitigation
  (pre-warm bootstrap via a moon dep before the parallel legs, or give each leg
  a genuinely isolated DEVENV_ROOT). Scoped to orion; other repos with parallel
  devenv evals in the gate stay exposed.
- **C:** both (A universal + B removes orion's false-confidence dead mitigation).

## Evidence locations

`src/` paths are this repo; `ci/` paths are the orion consumer repo (RigelBuild,
not carried into jj-hooks — cited to locate the consumer-side evidence).

- jj-hp worktree creation: `src/worktree.rs:40-55` (detached, no `.devenv/`
  since gitignored: `.gitignore:18` `.devenv*`).
- devenv self-regen + not-rewritten-when-present: proven empirically.
- orion ineffective mitigation: `ci/image/publish-ci-image.ts:287-308`, `:1159`.
- orion parallel legs in one gate: `ci/moon.yml` build-image-* tasks, run by
  `moon ci`.
