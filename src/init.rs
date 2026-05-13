//! `jj-hooks init` — interactive setup for the user-level config.
//!
//! Two yes/no prompts:
//! 1. Install a `jj push` alias that delegates to `jj-hooks push`.
//! 2. Auto-advance bookmarks when hooks modify files.
//!
//! Both write to the user-level jj config. Prompts go through the
//! [`Prompter`] trait so tests can script answers.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{JjHooksError, Result};
use crate::runner::Runner;

pub trait Prompter {
    fn confirm(&mut self, message: &str, default: bool) -> Result<bool>;
}

/// A prompter that returns pre-canned answers in order. Used in tests.
pub struct ScriptedPrompter {
    answers: std::vec::IntoIter<bool>,
}

impl ScriptedPrompter {
    pub fn new(answers: Vec<bool>) -> Self {
        Self {
            answers: answers.into_iter(),
        }
    }
}

impl Prompter for ScriptedPrompter {
    fn confirm(&mut self, _message: &str, default: bool) -> Result<bool> {
        Ok(self.answers.next().unwrap_or(default))
    }
}

/// Interactive prompter backed by dialoguer.
pub struct InteractivePrompter;

impl Prompter for InteractivePrompter {
    fn confirm(&mut self, message: &str, default: bool) -> Result<bool> {
        dialoguer::Confirm::new()
            .with_prompt(message)
            .default(default)
            .interact()
            .map_err(|e| JjHooksError::Io(std::io::Error::other(e.to_string())))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitPlan {
    pub install_alias: bool,
    pub advance_bookmarks: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitOutcome {
    pub alias_set: bool,
    pub advance_bookmarks_set: bool,
}

/// Build an [`InitPlan`] by asking the user (via `prompter`) which optional
/// integrations to install. `detected_runner` is for informational printing
/// — the plan itself is the same regardless.
pub fn plan(detected_runner: Option<Runner>, prompter: &mut dyn Prompter) -> Result<InitPlan> {
    if let Some(runner) = detected_runner {
        tracing::info!("detected hook runner: {}", runner.bin());
    } else {
        tracing::info!("no hook-runner config detected at workspace root");
    }

    let install_alias = prompter.confirm(
        "Set up `jj push` alias so it runs hooks before pushing?",
        false,
    )?;
    let advance_bookmarks = prompter.confirm(
        "Auto-advance bookmarks to fixup commits when hooks modify files?",
        false,
    )?;

    Ok(InitPlan {
        install_alias,
        advance_bookmarks,
    })
}

/// Apply an [`InitPlan`] by invoking `jj config set --user` for each
/// requested key.
///
/// If `config_path` is `Some`, JJ_CONFIG is set to that path for the
/// subprocess so the writes are scoped (used in tests).
pub fn apply(plan: &InitPlan, config_path: Option<&Path>) -> Result<InitOutcome> {
    let mut outcome = InitOutcome {
        alias_set: false,
        advance_bookmarks_set: false,
    };

    if plan.install_alias {
        jj_config_set(
            "aliases.push",
            r#"["util", "exec", "--", "jj-hooks", "push"]"#,
            config_path,
        )?;
        outcome.alias_set = true;
    }

    if plan.advance_bookmarks {
        jj_config_set("jj-hooks.advance-bookmarks", "true", config_path)?;
        outcome.advance_bookmarks_set = true;
    }

    Ok(outcome)
}

fn jj_config_set(key: &str, value: &str, config_path: Option<&Path>) -> Result<()> {
    let mut cmd = Command::new("jj");
    cmd.args(["config", "set", "--user", key, value]);

    if let Some(path) = config_path {
        cmd.env("JJ_CONFIG", path);
    }

    let output = cmd.output()?;
    if !output.status.success() {
        return Err(JjHooksError::JjFailed {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

/// Helper used by tests to construct a fully-qualified config path.
#[doc(hidden)]
pub fn _test_helper_unused(_p: PathBuf) {}
