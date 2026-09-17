{
  pkgs,
  lib,
  inputs,
  ...
}:
# jj-hooks dev shell. The shared toolchain (rust-overlay pin, linters, cargo-nextest, jj,
# and the ci:markdownlint/actionlint/nixfmt/deadnix lint task set) comes from the dev-shared
# module imported in devenv.yaml. This file adds only what is jj-hooks-specific: the hook
# framework backends its integration tests drive, the pkl package-cache warm, the crate's
# own ci:* tasks, and the hk pre-push gate install.
let
  # dev-shared puts jj-hooks on PATH from a released tag, which is wrong in this repo: the
  # shell would serve the last release while you edit the next one. Rebuild from the working
  # tree instead. `src = ./.` with `cargoLock.lockFile` needs no hash and no IFD, and it
  # breaks the dev-shared -> jj-hooks version cycle a flake input would create.
  #
  # hiPrio wins the PATH collision with the pinned build: devenv orders `packages` by
  # meta.priority, so the raw pin can otherwise shadow this one regardless of list order.
  jj-hooks-local = lib.hiPrio (
    pkgs.rustPlatform.buildRustPackage {
      pname = "jj-hooks";
      version = (lib.importTOML ./Cargo.toml).package.version;
      # An allowlist, not `./.`: a bare path literal ignores .gitignore, so it would
      # drag target/ (2+ GB) and .jj/ into the store and rebuild whenever either churns.
      src = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions [
          ./Cargo.toml
          ./Cargo.lock
          ./src
        ];
      };
      cargoLock.lockFile = ./Cargo.lock;
      # `cargo nextest` in ci:test gates the suite against real jj repos and hook backends;
      # this build only needs the two binaries.
      doCheck = false;
    }
  );
in
{
  packages = with pkgs; [
    jj-hooks-local

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
    "ci:script".exec =
      "python3 .github/scripts/test_bump-formulae.py && python3 .github/scripts/test_push-tap.py";
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
    _pkl_warm_ok=0
    for _ in 1 2 3; do
      ${pkgs.pkl}/bin/pkl download-package \
        "package://github.com/jdx/hk/releases/download/v1.48.0/hk@1.48.0" \
        >/dev/null 2>&1 && { _pkl_warm_ok=1; break; }
      sleep 2
    done
    [ "$_pkl_warm_ok" = 1 ] || echo "devenv: pkl hk-package warm failed; jj-hooks hk tests may fetch at runtime"
    unset _pkl_warm_ok
  '';
}
