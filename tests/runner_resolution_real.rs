//! Real-binary integration tests for the resolver's layers 1, 2, 3.
//!
//! Mechanism-level tests in `tests/push.rs` (`resolver_*`) and unit
//! tests in `src/runner.rs` prove that `resolve_runner_argv` assembles
//! the right argv prefix. Those use stub scripts as fake runners —
//! enough to catch typos in the argv-splicing logic, but not enough
//! to catch (say) a wrong `--hook-stage` flag that the resolver
//! would silently pass through to a real binary that then rejects
//! the args.
//!
//! These tests close the gap: each one stages a real prek / pre-commit
//! / uv binary at a path that isn't reachable through the sandbox's
//! PATH allowlist, then asserts a full `jj-hp push` succeeds — meaning
//! the resolver picked the right path AND the resulting argv was
//! something the real binary actually accepted.
//!
//! Each test gates on the relevant binary being present on the host's
//! parent PATH; if it isn't, the test early-returns (skips). Running
//! `cargo nextest run -p jj-hooks runner_resolution_real` on a host
//! with all three installed exercises everything; in restricted CI
//! environments the tests gracefully skip rather than fail.

mod harness;

use harness::{PRE_PUSH_PASSING, TestRepo, show};

/// Set up a push-ready repo with a passing pre-commit-config that
/// uses `entry: 'true'` (so the real runner just invokes /bin/true
/// and exits 0). Returns the head of the bookmark that's about to
/// be pushed.
fn ready_repo() -> TestRepo {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_PASSING);

    repo.write("hello.txt", "hello\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));
    repo
}

/// Skip-helper. Prints a skip notice to stderr (which cargo test
/// surfaces under `--nocapture`) and returns true when the binary
/// the test needs isn't installed on the parent PATH. Tests early-
/// return on `true` to skip.
fn skip_if_missing(name: &str) -> bool {
    if TestRepo::find_on_parent_path(name).is_none() {
        eprintln!(
            "test skipped: `{name}` not on parent PATH \
             (real-binary resolver test requires a host install)"
        );
        return true;
    }
    false
}

/// The set of "auxiliary" binaries the real runners may need to fork
/// when executing a `language: system` hook. `true` and `false` are
/// the hook entries; `git` is for diff computation; `jj`/`sh` are
/// for jj-hp's own bookkeeping. Keep this list small but complete —
/// missing entries surface as cryptic "command not found" inside the
/// runner's report rather than as a sandbox PATH violation.
const RUNNER_AUX_BINS: &[&str] = &["git", "jj", "sh", "true", "false"];

// -- Layer 1: jj-hooks.runner-bin config --------------------------------------

#[test]
fn resolver_layer1_real_prek_via_config_override() {
    if skip_if_missing("prek") {
        return;
    }
    let repo = ready_repo();

    // Stage a real prek at a path that isn't on the sandbox's PATH.
    let venv = tempfile::TempDir::new().unwrap();
    let real_prek = repo
        .stage_external_binary("prek", venv.path(), /*symlink_only=*/ false)
        .expect("prek must be on parent PATH (checked above)");

    // JJ_CONFIG sets the override + identity (the env_clear sandbox
    // loses any user-level identity jj might have).
    let jj_config = repo.tmp.path().join("jj-config.toml");
    std::fs::write(
        &jj_config,
        format!(
            r#"[user]
name = "jj-hooks tests"
email = "tests@jj-hooks.invalid"

[jj-hooks.runner-bin]
prek = "{}"
"#,
            real_prek.display()
        ),
    )
    .unwrap();

    // Sandboxed PATH includes the aux bins but NO `prek` — so the
    // only way the test succeeds is if layer 1 resolved the config
    // override and the real prek then drove the hook to a pass.
    let bin_dir = repo.sandbox_bin_dir();
    let _ = std::fs::remove_dir_all(&bin_dir);
    std::fs::create_dir(&bin_dir).unwrap();
    for name in RUNNER_AUX_BINS {
        let src = TestRepo::find_on_parent_path(name)
            .unwrap_or_else(|| panic!("{name} not on parent PATH"));
        std::os::unix::fs::symlink(&src, bin_dir.join(name)).unwrap();
    }

    let jj_hooks_bin = env!("CARGO_BIN_EXE_jj-hooks");
    let out = std::process::Command::new(jj_hooks_bin)
        .args([
            "--runner", "prek", "push", "--stage", "pre-push", "-b", "main",
        ])
        .current_dir(repo.primary())
        .env_clear()
        .env("PATH", &bin_dir)
        .env("JJ_CONFIG", &jj_config)
        .env("PRE_COMMIT_HOME", repo.tmp.path().join("pre-commit-home"))
        .env("HOME", repo.tmp.path())
        .env("JJ_HOOKS_LOG", "info")
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "real-prek-via-config push should succeed:\n{}",
        show(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("could not be resolved"),
        "layer 1 should have resolved prek, stderr:\n{stderr}"
    );
}

// -- Layer 2: .git/hooks/<stage> shim ----------------------------------------

#[test]
fn resolver_layer2_real_prek_via_install_shim() {
    if skip_if_missing("prek") {
        return;
    }
    let repo = ready_repo();

    let venv = tempfile::TempDir::new().unwrap();
    let real_prek = repo
        .stage_external_binary("prek", venv.path(), false)
        .expect("prek must be on parent PATH");
    repo.write_prek_shim("pre-push", &real_prek);

    // No JJ_CONFIG override — layer 1 misses, layer 2 must fire.
    let bin_dir = repo.sandbox_bin_dir();
    let _ = std::fs::remove_dir_all(&bin_dir);
    std::fs::create_dir(&bin_dir).unwrap();
    for name in RUNNER_AUX_BINS {
        let src = TestRepo::find_on_parent_path(name)
            .unwrap_or_else(|| panic!("{name} not on parent PATH"));
        std::os::unix::fs::symlink(&src, bin_dir.join(name)).unwrap();
    }

    // We still need a JJ_CONFIG with identity (env_clear strips user
    // config) but it must NOT contain a runner-bin override — proves
    // that layer 2 fired, not layer 1.
    let jj_config = repo.tmp.path().join("jj-config.toml");
    std::fs::write(
        &jj_config,
        r#"[user]
name = "jj-hooks tests"
email = "tests@jj-hooks.invalid"
"#,
    )
    .unwrap();

    let jj_hooks_bin = env!("CARGO_BIN_EXE_jj-hooks");
    let out = std::process::Command::new(jj_hooks_bin)
        .args([
            "--runner", "prek", "push", "--stage", "pre-push", "-b", "main",
        ])
        .current_dir(repo.primary())
        .env_clear()
        .env("PATH", &bin_dir)
        .env("JJ_CONFIG", &jj_config)
        .env("PRE_COMMIT_HOME", repo.tmp.path().join("pre-commit-home"))
        .env("HOME", repo.tmp.path())
        .env("JJ_HOOKS_LOG", "info")
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "real-prek-via-shim push should succeed:\n{}",
        show(&out)
    );
}

#[test]
fn resolver_layer2_real_pre_commit_via_install_shim() {
    // Most fragile of the three layer-2 tests: needs a Python
    // interpreter that has `pre_commit` importable as a module
    // (so `python -mpre_commit run …` actually drives the real
    // pre-commit code). On a typical pip-installed pre-commit
    // setup this is the venv's `python`; on Nix-wrapped
    // pre-commit, the bundled interpreter uses `site.addsitedir`
    // via a wrapper script and won't see pre_commit from a
    // module invocation.
    //
    // We probe at test time: try `python -mpre_commit --version`
    // against the host's python3. If that works, run the real
    // test; otherwise skip with a clear message.
    //
    // The probe must strip PYTHONPATH: the real push below runs the
    // interpreter under `env_clear()`, so `pre_commit` has to be
    // importable from python3's own sys.path — not via an ambient
    // PYTHONPATH. A Nix devenv shell exports a PYTHONPATH pointing at
    // pre-commit's site-packages, which would make the probe pass while
    // the env_clear'd real run fails; removing it here keeps the probe
    // an honest predictor (skip, not fail, when the module isn't real).
    if skip_if_missing("python3") {
        return;
    }
    let python3 = TestRepo::find_on_parent_path("python3").unwrap();
    let probe = std::process::Command::new(&python3)
        .args(["-mpre_commit", "--version"])
        .env_remove("PYTHONPATH")
        .output()
        .unwrap();
    if !probe.status.success() {
        eprintln!(
            "test skipped: `python3 -mpre_commit --version` failed on this host \
             (typically because pre-commit is installed via a wrapper script \
             rather than as a Python module reachable from python3's sys.path). \
             Skip is correct behaviour — layer 2 pre-commit support is exercised \
             by the synthetic test in tests/push.rs."
        );
        return;
    }

    let repo = ready_repo();

    // Stage `python3` itself as the venv interpreter. The shim's
    // INSTALL_PYTHON= points at it; jj-hp will spawn
    // `<staged-python> -mpre_commit run …`.
    let venv = tempfile::TempDir::new().unwrap();
    let staged_python = repo
        .stage_external_binary("python3", venv.path(), /*symlink_only=*/ true)
        .expect("python3 must be on parent PATH");
    repo.write_pre_commit_shim("pre-push", &staged_python);

    let bin_dir = repo.sandbox_bin_dir();
    let _ = std::fs::remove_dir_all(&bin_dir);
    std::fs::create_dir(&bin_dir).unwrap();
    for name in RUNNER_AUX_BINS {
        let src = TestRepo::find_on_parent_path(name)
            .unwrap_or_else(|| panic!("{name} not on parent PATH"));
        std::os::unix::fs::symlink(&src, bin_dir.join(name)).unwrap();
    }

    let jj_config = repo.tmp.path().join("jj-config.toml");
    std::fs::write(
        &jj_config,
        r#"[user]
name = "jj-hooks tests"
email = "tests@jj-hooks.invalid"
"#,
    )
    .unwrap();

    let jj_hooks_bin = env!("CARGO_BIN_EXE_jj-hooks");
    let out = std::process::Command::new(jj_hooks_bin)
        .args([
            "--runner",
            "pre-commit",
            "push",
            "--stage",
            "pre-push",
            "-b",
            "main",
        ])
        .current_dir(repo.primary())
        .env_clear()
        .env("PATH", &bin_dir)
        .env("JJ_CONFIG", &jj_config)
        .env("PRE_COMMIT_HOME", repo.tmp.path().join("pre-commit-home"))
        .env("HOME", repo.tmp.path())
        .env("JJ_HOOKS_LOG", "info")
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "real-pre-commit-via-shim push should succeed:\n{}",
        show(&out)
    );
}

// -- Layer 3: uv run -- ------------------------------------------------------

#[test]
fn resolver_layer3_real_prek_via_uv_run() {
    if skip_if_missing("uv") || skip_if_missing("prek") || skip_if_missing("python3") {
        return;
    }

    let repo = ready_repo();

    // Build a minimal uv-managed project at the workspace root:
    //   pyproject.toml + uv.lock + .venv/bin/<python,prek>
    // The .venv is pre-populated so uv doesn't need network access
    // to resolve / fetch anything — important for hermetic tests
    // that don't assume CI has PyPI reachable.
    let workspace = repo.primary();
    std::fs::write(
        workspace.join("pyproject.toml"),
        r#"[project]
name = "jj-hooks-resolver-test"
version = "0.0.0"
description = "scaffold"
requires-python = ">=3.9"
dependencies = []
"#,
    )
    .unwrap();
    // Minimal uv.lock — uv only needs the file to exist for the
    // resolver gate to fire; uv itself parses and accepts this shape.
    std::fs::write(
        workspace.join("uv.lock"),
        r#"version = 1
revision = 3
requires-python = ">=3.9"

[[package]]
name = "jj-hooks-resolver-test"
version = "0.0.0"
source = { virtual = "." }
"#,
    )
    .unwrap();

    // Pre-create .venv with the host's python3 + a copy of real prek.
    // We do this with `python3 -m venv` (stdlib, no network) rather
    // than `uv venv` (which may try to download a matching Python).
    let venv = workspace.join(".venv");
    let python3 = TestRepo::find_on_parent_path("python3").unwrap();
    let venv_status = std::process::Command::new(&python3)
        .args(["-m", "venv"])
        .arg(&venv)
        .status()
        .unwrap();
    assert!(venv_status.success(), "python3 -m venv failed");
    let _ = repo
        .stage_external_binary("prek", &venv.join("bin"), /*symlink_only=*/ false)
        .expect("prek must be on parent PATH");

    // Sandbox PATH: must include `uv` (the resolver invokes it) and
    // the aux bins. Crucially NOT `prek` — if jj-hp fell through to
    // layer 4 it'd fail; we want layer 3 to fire.
    let bin_dir = repo.sandbox_bin_dir();
    let _ = std::fs::remove_dir_all(&bin_dir);
    std::fs::create_dir(&bin_dir).unwrap();
    for name in RUNNER_AUX_BINS.iter().chain(["uv", "python3"].iter()) {
        let src = TestRepo::find_on_parent_path(name)
            .unwrap_or_else(|| panic!("{name} not on parent PATH"));
        std::os::unix::fs::symlink(&src, bin_dir.join(name)).unwrap();
    }

    let jj_config = repo.tmp.path().join("jj-config.toml");
    std::fs::write(
        &jj_config,
        r#"[user]
name = "jj-hooks tests"
email = "tests@jj-hooks.invalid"
"#,
    )
    .unwrap();

    let jj_hooks_bin = env!("CARGO_BIN_EXE_jj-hooks");
    let out = std::process::Command::new(jj_hooks_bin)
        .args([
            "--runner", "prek", "push", "--stage", "pre-push", "-b", "main",
        ])
        .current_dir(repo.primary())
        .env_clear()
        .env("PATH", &bin_dir)
        .env("JJ_CONFIG", &jj_config)
        .env("PRE_COMMIT_HOME", repo.tmp.path().join("pre-commit-home"))
        .env("HOME", repo.tmp.path())
        .env("JJ_HOOKS_LOG", "info")
        // uv writes to ~/.cache by default; scope to the tmpdir so
        // we don't pollute the host cache.
        .env("XDG_CACHE_HOME", repo.tmp.path().join("xdg-cache"))
        // Force uv to operate fully offline. The .venv is pre-staged
        // so this should never need to fetch anything; --offline
        // makes that explicit and fails fast if assumed otherwise.
        .env("UV_OFFLINE", "1")
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "real-prek-via-uv-run push should succeed:\n{}",
        show(&out)
    );
}
