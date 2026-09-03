//! Pre-hook setup steps run inside the ephemeral worktree.
//!
//! When the user declares `jj-hooks.setup` in jj config (user- or
//! repo-level), each step's command runs inside the freshly-created
//! `git worktree add --detach` checkout before the hook runner
//! fires. The motivating use case (issue #9) is `node_modules`:
//! hooks like `tsc` need install-time resources that aren't in the
//! committed tree, so the worktree starts without them and the
//! hook fails with `command not found`. A user-declared setup
//! command (`bun install`, `pnpm install --frozen-lockfile`, etc.)
//! restores them.
//!
//! Config shape (matches the precedent set by pre-commit / hk
//! per-step tables):
//!
//! ```toml
//! [[jj-hooks.setup]]
//! name = "install deps"          # optional; falls back to run[0]
//! run = ["bun", "install", "--frozen-lockfile"]
//!
//! [[jj-hooks.setup]]
//! run = ["bun", "run", "prepare"]
//! ```
//!
//! A non-zero exit from any step aborts the pipeline before the
//! hook runner is invoked — `run_once` propagates the failure
//! through the same path as a failing hook.

use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use crate::error::{JjHooksError, Result};
use crate::jj::JjCli;

/// One entry in `jj-hooks.setup`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SetupStep {
    /// Optional label used in failure messages. Falls back to
    /// `run[0]` (the bare program name) when omitted.
    #[serde(default)]
    pub name: Option<String>,
    /// argv list — `Command::new(run[0]).args(&run[1..])`. We use
    /// argv (not a shell string) so quoting rules can't bite. For
    /// chained commands the user runs `bash -c "..."` explicitly.
    pub run: Vec<String>,
}

impl SetupStep {
    /// Label to print in failure messages — `name` if set,
    /// otherwise the bare program name.
    pub fn label(&self) -> &str {
        if let Some(name) = self.name.as_deref() {
            return name;
        }
        self.run.first().map_or("<empty>", String::as_str)
    }
}

/// Parse `[[jj-hooks.setup]]` array-of-tables from a TOML fragment.
///
/// Input is the value `jj config get jj-hooks.setup` prints, which
/// for an array-of-tables comes back as a TOML *array of inline
/// tables* on a single line, e.g.
/// `[{ run = ["bun", "install"] }, { run = ["echo", "done"] }]`.
pub fn parse_steps(toml_fragment: &str) -> Result<Vec<SetupStep>> {
    // Wrap the array in a dummy key so we can use toml::de's table
    // deserializer (which is the only one stable across all the
    // shapes jj config get spits out — array-of-tables, inline,
    // empty array, etc.).
    let wrapped = format!("setup = {toml_fragment}");
    let table: SetupConfig = toml::from_str(&wrapped).map_err(|e| {
        JjHooksError::Parse(format!(
            "jj-hooks.setup must be an array of tables with a `run` field: {e}"
        ))
    })?;
    Ok(table.setup)
}

#[derive(Debug, Deserialize)]
struct SetupConfig {
    #[serde(default)]
    setup: Vec<SetupStep>,
}

/// Load `jj-hooks.setup` from jj's config, returning `Ok(vec![])`
/// when the key isn't set (`jj config get` exits non-zero for a
/// missing key — that's the "no setup configured" path, not an
/// error).
pub fn load_steps(jj: &JjCli) -> Result<Vec<SetupStep>> {
    // We can't distinguish "key missing" from "jj failed for some
    // other reason" without parsing jj's stderr, but the common
    // failure is exactly "key missing" — anything else surfaces
    // as a parse error on the empty input, which is fine.
    let Ok(value) = jj.run(&["config", "get", "jj-hooks.setup"]) else {
        return Ok(vec![]);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(vec![]);
    }
    parse_steps(value)
}

/// Execute each setup step in `worktree`. Returns `Ok(())` when all
/// steps succeed. The first non-zero exit aborts the rest and
/// returns a [`JjHooksError::JjFailed`] carrying the step's
/// label + the captured stderr.
///
/// `workspace_root` is exposed to each subprocess via the
/// `JJ_HOOKS_WORKSPACE` env var so steps can resolve paths from
/// the user's invocation workspace (e.g. `cp -al
/// "$JJ_HOOKS_WORKSPACE/node_modules" .` to hardlink-copy the
/// already-installed deps instead of running a full install).
/// Run each `[[jj-hooks.setup]]` step inside `worktree` in order.
///
/// **Always captures** stdout+stderr of every step regardless of any
/// caller-side capture preference. The motivation is the daily noise
/// problem: a successful `bun install` dumps eight lines of "+
/// package@x" that drown out the hook progress underneath. Surfacing
/// that output unconditionally was the original behavior; we now hide
/// it on success and only show it when something the user actually
/// needs to see happened (a failure, or `--verbose`).
///
/// Returns the concatenated captured output across all steps so the
/// caller can either drop it (success path) or fold it into the
/// per-bookmark captured buffer (verbose / failure path). On a
/// non-zero exit the captured bytes are bundled into the
/// [`JjHooksError::SetupFailed`] variant so the failure message has
/// real context attached instead of just "exited with status 1".
pub fn run_steps(steps: &[SetupStep], worktree: &Path, workspace_root: &Path) -> Result<String> {
    let mut captured = String::new();
    for step in steps {
        if step.run.is_empty() {
            return Err(JjHooksError::Parse(format!(
                "jj-hooks.setup step `{}` has empty `run` array",
                step.label()
            )));
        }

        tracing::info!("setup step `{}`: running {:?}", step.label(), step.run);

        // Always capture: success output is dropped, failure output
        // attaches to the error. Using `output()` (one combined read
        // after the child exits) is simpler than streaming and fine
        // here — setup steps are typically short (seconds-scale
        // package installs); there's no progress signal we'd lose
        // by waiting on `wait_with_output()`.
        let mut cmd = Command::new(&step.run[0]);
        cmd.args(&step.run[1..]).current_dir(worktree);
        // Merge the repo's direnv/devenv env (once-computed, cached) BEFORE
        // JJ_HOOKS_WORKSPACE so that variable always wins. No-op unless a
        // batch entrypoint populated the cache for this workspace_root.
        crate::repo_env::apply_repo_env(&mut cmd, workspace_root);
        // Point CARGO_TARGET_DIR at the primary `target/` AFTER apply_repo_env
        // so a repo-env-carried value can never win. No-op unless enabled.
        crate::gate_cache::apply_gate_cache(&mut cmd, workspace_root);
        let output = cmd.env("JJ_HOOKS_WORKSPACE", workspace_root).output()?;

        // Header line so the caller can tell which step produced
        // which captured output when there are multiple steps. Cheap
        // and the visual cost is one line per step in the eventual
        // dump — a fair price for being able to debug a multi-step
        // failure.
        captured.push_str(&format!("--- setup step `{}` ---\n", step.label()));
        captured.push_str(&String::from_utf8_lossy(&output.stdout));
        if !output.stderr.is_empty() {
            captured.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        if !captured.ends_with('\n') {
            captured.push('\n');
        }

        if !output.status.success() {
            return Err(JjHooksError::SetupFailed {
                name: step.label().to_owned(),
                status: output.status.code().unwrap_or(-1),
                captured,
            });
        }
    }
    Ok(captured)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_step_with_run_only() {
        // The shape `jj config get` prints when the user writes
        // `setup = [{ run = ["bun", "install"] }]`.
        let steps = parse_steps(r#"[{ run = ["bun", "install"] }]"#).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].run, vec!["bun", "install"]);
        assert_eq!(steps[0].name, None);
    }

    #[test]
    fn parse_named_step() {
        let steps = parse_steps(r#"[{ name = "deps", run = ["bun", "install"] }]"#).unwrap();
        assert_eq!(steps[0].name.as_deref(), Some("deps"));
        assert_eq!(steps[0].label(), "deps");
    }

    #[test]
    fn parse_multiple_steps_preserves_order() {
        let steps = parse_steps(
            r#"[
              { run = ["echo", "one"] },
              { name = "two", run = ["echo", "two"] },
              { run = ["echo", "three"] },
            ]"#,
        )
        .unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].run, vec!["echo", "one"]);
        assert_eq!(steps[1].label(), "two");
        assert_eq!(steps[2].run, vec!["echo", "three"]);
    }

    #[test]
    fn parse_empty_array_is_no_steps() {
        let steps = parse_steps("[]").unwrap();
        assert!(steps.is_empty());
    }

    #[test]
    fn parse_missing_run_field_errors() {
        // A step table without `run` is a user typo we should catch
        // at config-load time, not at execute time.
        let err = parse_steps(r#"[{ name = "no-run" }]"#).unwrap_err();
        assert!(
            matches!(err, JjHooksError::Parse(_)),
            "missing `run` should be a parse error, got: {err:?}"
        );
    }

    #[test]
    fn parse_rejects_bare_string_value() {
        // The "convenient" shape some users might try (a single
        // command instead of an array) should error clearly rather
        // than silently no-op.
        let err = parse_steps(r#""bun install""#).unwrap_err();
        assert!(matches!(err, JjHooksError::Parse(_)));
    }

    #[test]
    fn label_falls_back_to_first_argv_when_name_missing() {
        let step = SetupStep {
            name: None,
            run: vec!["bun".into(), "install".into()],
        };
        assert_eq!(step.label(), "bun");
    }

    #[test]
    fn label_prefers_name_over_argv() {
        let step = SetupStep {
            name: Some("install deps".into()),
            run: vec!["bun".into(), "install".into()],
        };
        assert_eq!(step.label(), "install deps");
    }

    #[test]
    #[cfg(unix)] // embeds the workspace path into an `sh -c` redirect; a Windows path breaks the shell command
    fn run_steps_passes_workspace_env_var() {
        // We can't easily intercept Command::env, but we *can* run
        // a real subprocess that records its env to a file. Using
        // /bin/sh keeps the test cheap (~1ms) and is portable to
        // any platform that has sh — which is every CI runner we
        // ship to.
        let tmp = tempfile::TempDir::new().unwrap();
        let out = tmp.path().join("env.txt");
        let step = SetupStep {
            name: None,
            run: vec![
                "sh".into(),
                "-c".into(),
                format!("printf %s \"$JJ_HOOKS_WORKSPACE\" > {}", out.display()),
            ],
        };
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        run_steps(&[step], tmp.path(), &workspace).unwrap();
        let captured = std::fs::read_to_string(&out).unwrap();
        assert_eq!(captured, workspace.display().to_string());
    }

    #[test]
    fn run_steps_aborts_on_first_failure() {
        // Step 1 fails; step 2 must NOT run. We assert step 2
        // didn't run by checking that its side-effect file is
        // absent after the call.
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("step2_ran");
        let steps = vec![
            SetupStep {
                name: Some("step1".into()),
                run: vec!["false".into()],
            },
            SetupStep {
                name: Some("step2".into()),
                run: vec![
                    "sh".into(),
                    "-c".into(),
                    format!("touch {}", marker.display()),
                ],
            },
        ];
        let err = run_steps(&steps, tmp.path(), tmp.path()).unwrap_err();
        let JjHooksError::SetupFailed { name, .. } = err else {
            panic!("expected SetupFailed, got {err:?}");
        };
        assert!(
            name.contains("step1"),
            "abort message should name the failed step `step1`: {name}"
        );
        assert!(
            !marker.exists(),
            "step2 must not run after step1 fails (marker file present)",
        );
    }

    #[test]
    fn run_steps_empty_run_array_errors() {
        let steps = vec![SetupStep {
            name: Some("bad".into()),
            run: vec![],
        }];
        let err = run_steps(
            &steps,
            std::env::temp_dir().as_path(),
            std::env::temp_dir().as_path(),
        )
        .unwrap_err();
        assert!(matches!(err, JjHooksError::Parse(_)));
    }

    #[test]
    fn run_steps_no_steps_is_silent_ok() {
        run_steps(
            &[],
            std::env::temp_dir().as_path(),
            std::env::temp_dir().as_path(),
        )
        .unwrap();
    }

    #[test]
    fn run_steps_captures_stdout_and_stderr_on_failure() {
        // Step writes a marker line on stdout AND stderr, then exits
        // non-zero. The captured field on the error must contain both
        // so the caller can show real context — not just "exited 1".
        let tmp = tempfile::TempDir::new().unwrap();
        let step = SetupStep {
            name: Some("loud-fail".into()),
            run: vec![
                "sh".into(),
                "-c".into(),
                "echo stdout-line; echo stderr-line >&2; exit 7".into(),
            ],
        };
        let err = run_steps(&[step], tmp.path(), tmp.path()).unwrap_err();
        let JjHooksError::SetupFailed {
            name,
            status,
            captured,
        } = err
        else {
            panic!("expected SetupFailed, got {err:?}");
        };
        assert_eq!(name, "loud-fail");
        assert_eq!(status, 7);
        assert!(
            captured.contains("stdout-line"),
            "captured should include stdout: {captured:?}",
        );
        assert!(
            captured.contains("stderr-line"),
            "captured should include stderr: {captured:?}",
        );
        // The header line names the step so the caller can tell which
        // step in a multi-step pipeline produced which output.
        assert!(
            captured.contains("loud-fail"),
            "captured should include step header: {captured:?}",
        );
    }

    #[test]
    fn run_steps_returns_captured_output_on_success() {
        // Success path should still return the captured bytes so the
        // caller can fold them into the per-bookmark buffer when
        // `--verbose` is on. Default callers throw the return value
        // away.
        let tmp = tempfile::TempDir::new().unwrap();
        let step = SetupStep {
            name: Some("chatty".into()),
            run: vec![
                "sh".into(),
                "-c".into(),
                "echo hello-world; echo on-stderr >&2".into(),
            ],
        };
        let captured = run_steps(&[step], tmp.path(), tmp.path()).unwrap();
        assert!(
            captured.contains("hello-world"),
            "stdout missing: {captured:?}",
        );
        assert!(
            captured.contains("on-stderr"),
            "stderr missing: {captured:?}",
        );
    }

    #[test]
    fn run_steps_silences_chatty_success_from_terminal() {
        // Regression guard: a successful step must NOT write to the
        // parent's stdout/stderr. We can't easily intercept the
        // parent's terminal in a unit test, but we can verify the
        // child was spawned with captured pipes by checking the
        // captured buffer holds the output that would otherwise have
        // gone to the terminal. (Before this change, `bun install`
        // would dump its banner to the user's screen on every push.)
        let tmp = tempfile::TempDir::new().unwrap();
        let step = SetupStep {
            name: None,
            run: vec!["echo".into(), "this-must-not-leak-to-terminal".into()],
        };
        let captured = run_steps(&[step], tmp.path(), tmp.path()).unwrap();
        assert!(captured.contains("this-must-not-leak-to-terminal"));
    }

    #[test]
    fn run_steps_applies_repo_env_patch() {
        // Twin of the run_subprocess test: a seeded patch must reach setup
        // steps too. RED before T2 (run_steps set only JJ_HOOKS_WORKSPACE).
        let ws = tempfile::TempDir::new().unwrap();
        crate::repo_env::test_seed(
            ws.path(),
            crate::repo_env::EnvPatch::Patch(std::collections::HashMap::from([(
                "REPO_ENV_MARKER".to_string(),
                Some("applied".to_string()),
            )])),
        );
        let step = SetupStep {
            name: None,
            run: vec![
                "sh".into(),
                "-c".into(),
                "printf 'MARK=[%s]' \"${REPO_ENV_MARKER:-unset}\"".into(),
            ],
        };
        let captured = run_steps(&[step], ws.path(), ws.path()).unwrap();
        assert!(
            captured.contains("MARK=[applied]"),
            "captured: {captured:?}"
        );
    }

    #[test]
    fn run_steps_strips_git_local_env_from_patch() {
        let ws = tempfile::TempDir::new().unwrap();
        crate::repo_env::test_seed(
            ws.path(),
            crate::repo_env::EnvPatch::Patch(std::collections::HashMap::from([(
                "GIT_DIR".to_string(),
                Some("/bogus/primary/.git".to_string()),
            )])),
        );
        let step = SetupStep {
            name: None,
            run: vec![
                "sh".into(),
                "-c".into(),
                "printf 'GD=[%s]' \"${GIT_DIR:-unset}\"".into(),
            ],
        };
        let captured = run_steps(&[step], ws.path(), ws.path()).unwrap();
        assert!(captured.contains("GD=[unset]"), "captured: {captured:?}");
    }
}
