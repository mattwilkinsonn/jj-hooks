//! Point the gate subprocess's `CARGO_TARGET_DIR` at the PRIMARY workspace's
//! `target/`, so the gate reuses the user's own warm dev builds instead of
//! paying a cold from-scratch build in the ephemeral `/tmp` worktree (whose
//! cargo `target/` is empty).
//!
//! `jj-hp` runs the pre-push gate (`moon ci` via hk) inside a fresh detached
//! worktree under `/tmp`. That worktree's `target/` starts empty, so even a
//! correctly-scoped, sccache-backed build still pays a from-scratch link +
//! incremental-cache miss (~35s measured) that a warm `target/` closes to a
//! ~2-3s incremental build. This module injects `CARGO_TARGET_DIR =
//! <primary>/target` into the gate `Command` — AFTER [`crate::repo_env`] so a
//! repo-env-carried value can never win, and alongside `JJ_HOOKS_WORKSPACE`.
//!
//! The injection is unconditional (not Rust-gated): nothing in a JS/nix gate
//! reads `CARGO_TARGET_DIR`, so it is harmless for non-cargo repos, and a
//! non-Rust-primary repo that later regrows first-party Rust needs no revisit.
//! An opt-out exists for users who want the gate isolated from their primary
//! `target/` (env `JJ_HOOKS_NO_GATE_CACHE`, jj config `jj-hooks.gate-cache =
//! "off"`), read ONCE at each batch entrypoint (mirroring
//! [`crate::repo_env::repo_env_enabled`]) and stored in a process-global cache
//! that the spawn sites read back via [`apply_gate_cache`]. When opted out,
//! [`apply_gate_cache`] is a strict no-op — it does NOT clear an inherited
//! value, so the child env is byte-identical to the pre-fix behavior.
//! See `docs/designs/tools/jj-hp-gate-worktree-cost.md` (Mode A / T1).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

/// Process-global cache of the gate-cache opt-out decision, keyed by
/// canonicalized `workspace_root`. Populated once per process under the mutex
/// at each batch entrypoint (mirroring [`crate::repo_env`]'s cache); spawn
/// sites only read it.
fn cache() -> &'static Mutex<HashMap<PathBuf, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Canonicalized cache key. Falls back to the raw path when canonicalization
/// fails (e.g. the directory was removed) so distinct roots still map to
/// distinct keys. Matches [`crate::repo_env`]'s `cache_key` convention.
fn cache_key(workspace_root: &Path) -> PathBuf {
    dunce::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf())
}

/// Record whether the gate-cache mechanism is enabled for `workspace_root`.
/// Called EAGERLY once at each batch entrypoint (mirroring
/// [`crate::repo_env::repo_env`]); the spawn sites read it back via
/// [`apply_gate_cache`], which has no `JjCli` in scope. `enabled` carries the
/// opt-outs, read once at the entrypoint (see [`gate_cache_enabled`]).
pub fn gate_cache(workspace_root: &Path, enabled: bool) {
    cache()
        .lock()
        .unwrap()
        .insert(cache_key(workspace_root), enabled);
}

/// Inject the gate-cache `CARGO_TARGET_DIR` into `cmd` if the mechanism is
/// enabled for `workspace_root` (per the process-global populated at the
/// entrypoint). Set to `<workspace_root>/target` — the PRIMARY repo's target
/// dir — so the gate reuses the user's warm dev builds.
///
/// MUST be called AFTER [`crate::repo_env::apply_repo_env`] so a
/// repo-env-carried `CARGO_TARGET_DIR` can never win over jj-hp's. When
/// disabled (or the cache is unpopulated), this is a strict no-op: it does NOT
/// clear an inherited value, keeping the child env byte-identical to pre-fix.
pub fn apply_gate_cache(cmd: &mut std::process::Command, workspace_root: &Path) {
    let enabled = {
        let guard = cache().lock().unwrap();
        guard.get(&cache_key(workspace_root)).copied()
    };
    // Unpopulated (non-batch path) or explicitly disabled — no-op, never a
    // clear: the child keeps whatever `CARGO_TARGET_DIR` it inherits.
    if enabled != Some(true) {
        return;
    }
    // Derive the injected dir from the SAME canonical path as the cache key,
    // so a non-canonical (symlinked/relative) primary root can't split the
    // key from the value.
    cmd.env("CARGO_TARGET_DIR", cache_key(workspace_root).join("target"));
}

/// Read the gate-cache opt-outs, in precedence order:
/// 1. `JJ_HOOKS_NO_GATE_CACHE` env var — any non-empty value disables.
/// 2. jj config `jj-hooks.gate-cache` — `"off"` disables; unset or `"auto"`
///    enables. An unrecognized non-empty value warns (tracing) and enables.
///    Read via the same `jj config get` pattern as
///    [`crate::repo_env::repo_env_enabled`].
///
/// Called ONCE at each batch entrypoint and passed into [`gate_cache`] as
/// `enabled`; never stored in an ambient global.
pub fn gate_cache_enabled(jj: &crate::jj::JjCli) -> bool {
    let env_optout = std::env::var_os("JJ_HOOKS_NO_GATE_CACHE");
    let config = jj.run(&["config", "get", "jj-hooks.gate-cache"]).ok();
    enabled_from(env_optout.as_deref(), config.as_deref())
}

/// Pure decision core of [`gate_cache_enabled`], split out so the precedence is
/// testable without a live jj repo. Strict parse: only `"off"` (case- and
/// space-insensitive) disables; unset or `"auto"` enables silently; any other
/// non-empty value warns (tracing) and enables.
fn enabled_from(env_optout: Option<&std::ffi::OsStr>, config: Option<&str>) -> bool {
    if let Some(value) = env_optout
        && !value.is_empty()
    {
        return false;
    }
    match config.map(str::trim) {
        Some(c) if c.eq_ignore_ascii_case("off") => false,
        None => true,
        Some(c) if c.is_empty() || c.eq_ignore_ascii_case("auto") => true,
        Some(c) => {
            tracing::warn!(
                "jj-hp: unrecognized value {c:?} for jj config `jj-hooks.gate-cache` \
                 (expected `off` or `auto`); gate cache stays enabled"
            );
            #[cfg(test)]
            WARN_COUNT.fetch_add(1, Ordering::SeqCst);
            true
        }
    }
}

#[cfg(test)]
static WARN_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Test-only: seed the process-global cache directly, so spawn-site tests
/// (`hooks.rs`, `setup.rs`) can assert [`apply_gate_cache`]'s effect on a real
/// `Command` without a live `JjCli`.
#[cfg(test)]
pub(crate) fn test_seed(workspace_root: &Path, enabled: bool) {
    cache()
        .lock()
        .unwrap()
        .insert(cache_key(workspace_root), enabled);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::process::{Command, Stdio};

    #[test]
    fn enabled_from_precedence() {
        // Env-var opt-out wins over any config value.
        assert!(!enabled_from(Some(OsStr::new("1")), None));
        assert!(!enabled_from(Some(OsStr::new("anything")), Some("auto")));
        // Empty env value does NOT disable.
        assert!(enabled_from(Some(OsStr::new("")), None));
        // Config "off" disables (case/space-insensitive).
        assert!(!enabled_from(None, Some("off")));
        assert!(!enabled_from(None, Some("  OFF\n")));
        // Unset / "auto" enable.
        assert!(enabled_from(None, None));
        assert!(enabled_from(None, Some("auto")));
    }

    #[test]
    fn unrecognized_config_value_warns_and_enables() {
        let before = WARN_COUNT.load(Ordering::SeqCst);
        assert!(
            enabled_from(None, Some("bananas")),
            "an unrecognized value must still enable"
        );
        assert_eq!(
            WARN_COUNT.load(Ordering::SeqCst),
            before + 1,
            "an unrecognized value must warn exactly once"
        );
    }

    #[test]
    fn apply_sets_cargo_target_dir_when_enabled() {
        let ws = tempfile::TempDir::new().unwrap();
        test_seed(ws.path(), true);
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("printf 'CTD=[%s]' \"${CARGO_TARGET_DIR:-unset}\"")
            .stdout(Stdio::piped());
        apply_gate_cache(&mut cmd, ws.path());
        let out = cmd.output().unwrap();
        // Canonicalize the expected path the same way the product does (via
        // `cache_key`), so the assertion holds on a host whose TMPDIR resolves
        // through a symlink (e.g. macOS `/tmp` -> `/private/tmp`).
        let expected_root = dunce::canonicalize(ws.path()).unwrap();
        let expected = format!("CTD=[{}]", expected_root.join("target").display());
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
    }

    #[test]
    fn apply_is_noop_when_disabled_and_keeps_inherited_value() {
        let ws = tempfile::TempDir::new().unwrap();
        test_seed(ws.path(), false);
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("printf 'CTD=[%s]' \"${CARGO_TARGET_DIR:-unset}\"")
            // A decoy inherited value must survive an opt-out unchanged.
            .env("CARGO_TARGET_DIR", "/decoy/target")
            .stdout(Stdio::piped());
        apply_gate_cache(&mut cmd, ws.path());
        let out = cmd.output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "CTD=[/decoy/target]",
            "opt-out must be byte-identical fallback: never clear an inherited value"
        );
    }

    #[test]
    fn apply_is_noop_for_unpopulated_cache() {
        let ws = tempfile::TempDir::new().unwrap(); // never seeded
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("printf 'CTD=[%s]' \"${CARGO_TARGET_DIR:-unset}\"")
            .env("CARGO_TARGET_DIR", "/decoy/target")
            .stdout(Stdio::piped());
        apply_gate_cache(&mut cmd, ws.path());
        let out = cmd.output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "CTD=[/decoy/target]",
            "an unpopulated cache must leave the inherited value untouched"
        );
    }
}
