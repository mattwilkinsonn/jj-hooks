//! Real-`direnv` integration tests for `repo_env`.
//!
//! The unit tests in `src/repo_env.rs` use a hermetic fake `direnv` shim to
//! prove the detection / retry / cache / apply logic. These tests exercise the
//! real `direnv export json` contract against a plain `export FOO=bar` `.envrc`
//! (no nix/devenv needed), guarding against direnv version drift in the export
//! and blocked-`.envrc` output formats (the blocked format has changed once
//! across direnv releases).
//!
//! Each test gates on `direnv` being present on the parent PATH; if it isn't,
//! the test early-returns (skips) rather than fails — mirroring
//! `runner_resolution_real.rs`.
//!
//! REQUIRES cargo-nextest (per-process test isolation). These tests mutate
//! process-global state — `XDG_*` and `DIRENV_*` via `std::env::set_var` — so
//! they are only isolated when each runs in its own process, which nextest
//! guarantees and plain `cargo test` does NOT (it runs a binary's tests on
//! shared threads, where the env mutations race). Run them with
//! `cargo nextest run -p jj-hooks --test repo_env_real`; CI is nextest-only
//! (`ci.yml`). Do not run this file under plain `cargo test`.

use std::path::Path;
use std::process::Command;

use jj_hooks::repo_env::{EnvPatch, repo_env};

/// Skip-helper: true when `direnv` isn't installed on the parent PATH.
fn skip_if_no_direnv() -> bool {
    let missing = std::env::var_os("PATH")
        .map(|path| !std::env::split_paths(&path).any(|dir| dir.join("direnv").is_file()))
        .unwrap_or(true);
    if missing {
        eprintln!("test skipped: `direnv` not on parent PATH");
    }
    missing
}

/// Point direnv's allow-list + config at a tempdir so `direnv allow` state is
/// hermetic and doesn't touch the host's real direnv data.
fn isolate_direnv_state(home: &Path) {
    // SAFETY: nextest runs each test in its own process, so no sibling test
    // observes these mutations.
    unsafe {
        std::env::set_var("XDG_DATA_HOME", home.join("data"));
        std::env::set_var("XDG_CONFIG_HOME", home.join("config"));
        // Ensure no stale DIRENV_* from the harness's own shell leaks in.
        std::env::remove_var("DIRENV_DIR");
        std::env::remove_var("DIRENV_FILE");
        std::env::remove_var("DIRENV_DIFF");
        std::env::remove_var("DIRENV_WATCHES");
    }
    std::fs::create_dir_all(home.join("data")).unwrap();
    std::fs::create_dir_all(home.join("config")).unwrap();
}

fn write_envrc(ws: &Path, body: &str) {
    std::fs::write(ws.join(".envrc"), body).unwrap();
}

fn direnv_allow(ws: &Path) {
    let out = Command::new("direnv")
        .arg("allow")
        .arg(ws.join(".envrc"))
        .current_dir(ws)
        .output()
        .expect("spawn direnv allow");
    assert!(
        out.status.success(),
        "direnv allow failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn blocked_envrc_with_autoallow_off_is_disabled() {
    if skip_if_no_direnv() {
        return;
    }
    let home = tempfile::TempDir::new().unwrap();
    isolate_direnv_state(home.path());
    let ws = tempfile::TempDir::new().unwrap();
    write_envrc(ws.path(), "export FOO=bar\n");
    // No `direnv allow`, and autoallow OFF — the `.envrc` stays blocked, so
    // repo_env degrades to Disabled (byte-identical to pre-Mode-B behavior,
    // and emits the visible eprintln checked in unit tests).
    let patch = repo_env(ws.path(), true, false);
    assert_eq!(*patch, EnvPatch::Disabled);
}

#[test]
fn blocked_envrc_with_autoallow_on_is_allowed_and_yields_patch() {
    if skip_if_no_direnv() {
        return;
    }
    let home = tempfile::TempDir::new().unwrap();
    isolate_direnv_state(home.path());
    let ws = tempfile::TempDir::new().unwrap();
    write_envrc(ws.path(), "export FOO=bar\n");
    // No manual `direnv allow` — with autoallow ON (the default), jj-hp runs
    // `direnv allow` itself, then the re-export yields a Patch carrying FOO.
    let patch = repo_env(ws.path(), true, true);
    let EnvPatch::Patch(map) = &*patch else {
        panic!("expected Patch after auto-allow, got {patch:?}");
    };
    assert_eq!(map.get("FOO"), Some(&Some("bar".to_string())));
    // The allow landed in the ISOLATED trust DB (XDG_DATA_HOME), not the
    // host's global one — proving the test is hermetic. direnv (2.x) records
    // allowed `.envrc`s under `<XDG_DATA_HOME>/direnv/allow/`; if a future
    // direnv relocates that dir this `read_dir` yields 0 and the assertion
    // fails LOUDLY (a false RED on upgrade), never a false green — the
    // functional `FOO=bar` check above already proves the allow itself worked.
    let allow_dir = home.path().join("data").join("direnv").join("allow");
    let recorded = std::fs::read_dir(&allow_dir)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert!(
        recorded > 0,
        "auto-allow must record the trust in the isolated DB at {}",
        allow_dir.display()
    );
}

#[test]
fn bare_env_export_emits_the_var() {
    if skip_if_no_direnv() {
        return;
    }
    let home = tempfile::TempDir::new().unwrap();
    isolate_direnv_state(home.path());
    let ws = tempfile::TempDir::new().unwrap();
    write_envrc(ws.path(), "export FOO=bar\n");
    direnv_allow(ws.path());
    // The harness process has no repo env loaded (bare), so the diff sets FOO.
    let patch = repo_env(ws.path(), true, false);
    let EnvPatch::Patch(map) = &*patch else {
        panic!("expected Patch, got {patch:?}");
    };
    assert_eq!(map.get("FOO"), Some(&Some("bar".to_string())));
}

#[test]
fn loaded_env_is_empty_patch() {
    if skip_if_no_direnv() {
        return;
    }
    let home = tempfile::TempDir::new().unwrap();
    isolate_direnv_state(home.path());
    let ws = tempfile::TempDir::new().unwrap();
    write_envrc(ws.path(), "export FOO=bar\n");
    direnv_allow(ws.path());

    // Simulate a shell that already has THIS repo's env loaded: run
    // `direnv export json` once to obtain the DIRENV_* state, apply it to the
    // process env, then repo_env's export should see no changes → empty diff.
    let out = Command::new("direnv")
        .arg("export")
        .arg("json")
        .current_dir(ws.path())
        .env("DIRENV_LOG_FORMAT", "")
        .output()
        .expect("spawn direnv export json");
    assert!(out.status.success());
    let diff: std::collections::HashMap<String, Option<String>> =
        serde_json::from_slice(&out.stdout).expect("parse direnv export");
    // SAFETY: per-process (nextest) isolation, see isolate_direnv_state.
    unsafe {
        for (k, v) in &diff {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    let patch = repo_env(ws.path(), true, false);
    let EnvPatch::Patch(map) = &*patch else {
        panic!("expected Patch (empty), got {patch:?}");
    };
    assert!(
        map.is_empty(),
        "already-loaded env should yield an empty diff, got: {map:?}"
    );
}
