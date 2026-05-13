set shell := ["bash", "-c"]
set dotenv-load := false

default:
    @just --list

# Install all supported hook runners. macOS uses Homebrew, Linux uses
# `uv` for pre-commit and prek, a curl download for lefthook, and
# `cargo binstall` (prebuilt binary) for hk.
install-deps:
    #!/usr/bin/env bash
    set -euo pipefail
    case "$(uname -s)" in
        Darwin)
            brew install pre-commit prek lefthook hk
            ;;
        Linux)
            mkdir -p "$HOME/.local/bin"
            export PATH="$HOME/.local/bin:$PATH"

            uv tool install pre-commit
            uv tool install prek

            arch="$(uname -m)"
            case "$arch" in
                x86_64)  lefthook_arch=x86_64 ;;
                aarch64) lefthook_arch=arm64 ;;
                *)
                    echo "unsupported Linux arch for lefthook: $arch" >&2
                    exit 1
                    ;;
            esac
            curl -fsSL "https://github.com/evilmartians/lefthook/releases/latest/download/lefthook_linux_${lefthook_arch}" \
                -o "$HOME/.local/bin/lefthook"
            chmod +x "$HOME/.local/bin/lefthook"

            # cargo binstall pulls prebuilt artifacts (much faster than
            # building from source). Bootstrap it if it's missing.
            if ! command -v cargo-binstall >/dev/null 2>&1; then
                cargo install cargo-binstall
            fi
            cargo binstall -y hk
            ;;
        *)
            echo "unsupported OS: $(uname -s)" >&2
            exit 1
            ;;
    esac

# Verify all four runners resolve on PATH.
check-deps:
    #!/usr/bin/env bash
    set -euo pipefail
    missing=()
    for bin in pre-commit prek lefthook hk; do
        if ! command -v "$bin" >/dev/null 2>&1; then
            missing+=("$bin")
        fi
    done
    if [ ${#missing[@]} -gt 0 ]; then
        echo "missing hook runners: ${missing[*]}" >&2
        echo "run \`just install-deps\` to install them" >&2
        exit 1
    fi
    echo "all hook runners installed: pre-commit prek lefthook hk"

build:
    cargo build --all-targets

# Run the full test suite. Requires `just install-deps` to have been run first.
test: check-deps
    cargo nextest run --no-fail-fast

# Run only unit / pure tests that don't need external binaries.
test-pure:
    cargo nextest run --no-fail-fast --test parse --test runner

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --all-targets -- -D warnings

# Pre-commit check: fmt + clippy + tests.
ci: fmt-check clippy test

# Install a debug build to ~/.cargo/bin. Codesigns on macOS so the binary
# can be re-run without confirmation. No --release.
install-debug:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build
    dest="${CARGO_HOME:-$HOME/.cargo}/bin"
    mkdir -p "$dest"
    # On Linux, writing over an in-use executable fails with ETXTBSY
    # (text file busy). Unlink first so a running process keeps its
    # inode while we drop a fresh one at the path. macOS lets you
    # overwrite an active binary, so the unlink is a no-op there.
    rm -f "$dest/jj-hooks"
    cp target/debug/jj-hooks "$dest/jj-hooks"
    if [[ "$(uname)" == "Darwin" ]]; then
        codesign -s - "$dest/jj-hooks" 2>/dev/null && echo "Codesigned jj-hooks" || true
    fi
    echo "Installed debug build to $dest/jj-hooks"

# Install a release build to ~/.cargo/bin. Codesigns on macOS.
install: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    dest="${CARGO_HOME:-$HOME/.cargo}/bin"
    mkdir -p "$dest"
    # See install-debug for the ETXTBSY rationale.
    rm -f "$dest/jj-hooks"
    cp target/release/jj-hooks "$dest/jj-hooks"
    if [[ "$(uname)" == "Darwin" ]]; then
        codesign -s - "$dest/jj-hooks" 2>/dev/null && echo "Codesigned jj-hooks" || true
    fi
    echo "Installed release build to $dest/jj-hooks"

build-release:
    cargo build --release

# Cut a release. Bumps Cargo.toml, refreshes Cargo.lock, commits the
# bump on top of @, tags @- with the version, and exports tags to git.
# Stops short of pushing — run `jj git push` to push the commit and
# `jj-push-tags vX.Y.Z` to push the tag (or `jj-push-tags --all`).
#
# Usage: just release v0.1.0
release VERSION:
    #!/usr/bin/env bash
    set -euo pipefail

    version="{{ VERSION }}"
    if [[ ! "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9._-]+)?$ ]]; then
        echo "error: VERSION must look like v1.2.3 or v1.2.3-rc.1 (got: $version)" >&2
        exit 1
    fi
    bare="${version#v}"

    # Require a clean @ — release commits should not include unrelated work.
    if [ -n "$(jj diff --summary --ignore-working-copy 2>/dev/null)" ]; then
        echo "error: working copy @ has uncommitted changes; finalize them first" >&2
        exit 1
    fi

    if jj --ignore-working-copy tag list -T 'name ++ "\n"' 2>/dev/null | grep -qx "$version"; then
        echo "error: tag $version already exists" >&2
        exit 1
    fi

    if ! cargo set-version --help >/dev/null 2>&1; then
        echo "error: cargo-edit not installed (run: cargo install --locked cargo-edit)" >&2
        exit 1
    fi

    echo "Setting package version to $bare…"
    cargo set-version "$bare"
    echo

    echo "Updating Cargo.lock…"
    cargo update --workspace
    echo

    echo "Committing release bump as a new jj change on top of @…"
    jj commit -m "release: $version"
    echo

    echo "Tagging @- with $version…"
    jj tag set "$version" -r @-
    echo

    echo "Exporting refs to git…"
    jj --ignore-working-copy git export >/dev/null 2>&1 || true
    echo

    echo "Done. To publish:"
    echo "  jj git push           # push the release-bump commit"
    echo "  jj-push-tags $version # push the tag (triggers release.yml)"

