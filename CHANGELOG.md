# Changelog

All notable changes to jj-hooks are tracked here.

## [Unreleased]

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
