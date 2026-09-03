{ pkgs, inputs, ... }:
# jj-hooks dev shell. The shared toolchain (rust-overlay pin, linters, cargo-nextest, jj,
# and the ci:markdownlint/actionlint/nixfmt/deadnix lint task set) comes from the dev-shared
# module imported in devenv.yaml. This file adds only what is jj-hooks-specific: the hook
# framework backends its integration tests drive, the pkl package-cache warm, the crate's
# own ci:* tasks, and the hk pre-push gate install.
{
  packages = with pkgs; [
    # jj-hooks' integration tests drive real hook frameworks, so the backends must be on
    # PATH: pre-commit, prek, lefthook, and hk (from its flake input); pkl (hk reads hk.pkl).
    pre-commit
    prek
    lefthook
    pkl
    inputs.hk.packages.${pkgs.stdenv.system}.hk
  ];

  # Crate checks. Named ci:* so `devenv tasks run ci` (a namespace-prefix selector) runs
  # them alongside the shared ci:markdownlint/actionlint/nixfmt/deadnix from dev-shared.
  # NEVER a bare task named `ci` — devenv rejects it ("names must be namespace:name").
  # Single crate, so no `-p` package filter needed.
  tasks = {
    "ci:fmt".exec = "cargo fmt --check";
    "ci:clippy".exec = "cargo clippy --all-targets -- -D warnings";
    "ci:test".exec = "cargo nextest run --no-fail-fast";
  };

  enterShell = ''
    # Install the pre-push git hook (a thin shell over `devenv tasks run ci`). hk install is
    # idempotent, so re-run it on entry to pick up hk.pkl changes.
    if command -v hk >/dev/null 2>&1; then
      hk install >/dev/null 2>&1 || echo "devenv: hk install failed; run 'hk install' to enable the pre-push gate"
    fi
    # Warm the pkl package cache for jj-hooks' hk integration tests: their fixtures `amends`
    # the hk pkl package from GitHub at test time, so a transient GitHub 502 flakes the suite.
    # Pre-fetch once here (a no-op once cached). Best-effort with retry; never blocks entry.
    for _ in 1 2 3; do
      ${pkgs.pkl}/bin/pkl download-package \
        "package://github.com/jdx/hk/releases/download/v1.48.0/hk@1.48.0" \
        >/dev/null 2>&1 && break
      sleep 2
    done || echo "devenv: pkl hk-package warm failed; jj-hooks hk tests may fetch at runtime"
  '';
}
