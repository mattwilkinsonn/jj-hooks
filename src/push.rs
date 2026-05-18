//! Push pipeline: dry-run parse → per-bookmark hook → push or abort.

use std::path::Path;
use std::process::Command;

use crate::bookmark_updates::{BookmarkUpdate, UpdateType, parse_git_push_dry_run};
use crate::error::{JjHooksError, Result};
use crate::hooks::{HookOutcome, run_for_update};
use crate::jj::{self, JjCli};
use crate::runner::{Runner, Stage};

#[derive(Debug, Clone)]
pub struct PushReport {
    /// Per-bookmark hook outcomes.
    pub per_bookmark: Vec<(BookmarkUpdate, HookOutcome)>,
    /// Set true if there was nothing to do (no config, no updates, or only
    /// deletes). The caller falls through to a plain `jj git push`.
    pub skipped: bool,
}

impl PushReport {
    pub fn any_failure(&self) -> bool {
        self.per_bookmark.iter().any(|(_, o)| !o.success)
    }

    pub fn any_fixup(&self) -> bool {
        self.per_bookmark
            .iter()
            .any(|(_, o)| o.fixup_commit.is_some())
    }
}

/// Run hooks for every bookmark that would be pushed by `jj git push <push_args>`.
///
/// `cli_runner` is `None` when the user did not pass `--runner`. In that
/// case `run_for_update` autodetects the runner from each target commit's
/// own tree (so a runner-migration commit picks the new runner). When
/// `Some(r)`, the user's choice is honored as-is for every update.
pub fn run_checks(
    jj: &JjCli,
    workspace_root: &Path,
    cli_runner: Option<Runner>,
    stage: Stage,
    push_args: &[String],
) -> Result<PushReport> {
    let updates = dry_run_updates(jj, push_args)?;

    if updates.is_empty() {
        tracing::info!("nothing to push, skipping hooks");
        return Ok(PushReport {
            per_bookmark: vec![],
            skipped: true,
        });
    }

    let non_deletes: Vec<_> = updates
        .into_iter()
        .filter(|u| u.update_type != UpdateType::Delete)
        .collect();

    if non_deletes.is_empty() {
        tracing::info!("only deletions to push, skipping hooks");
        return Ok(PushReport {
            per_bookmark: vec![],
            skipped: true,
        });
    }

    let primary_git_dir = jj::primary_git_dir(workspace_root)?;

    let mut per_bookmark = Vec::with_capacity(non_deletes.len());
    for update in non_deletes {
        match cli_runner {
            Some(r) => tracing::info!("{update}: running {} hooks", r.bin()),
            None => tracing::info!("{update}: autodetecting runner inside target worktree"),
        }
        let outcome = run_for_update(jj, &primary_git_dir, cli_runner, stage, &update)?;
        per_bookmark.push((update, outcome));
    }

    Ok(PushReport {
        per_bookmark,
        skipped: false,
    })
}

/// If `advance_bookmarks` is true, point each local bookmark with a fixup
/// commit at that fixup commit, and forget the temporary
/// `jj-hooks-fixup/<name>` bookmark that `jj git import` created when it
/// picked up our `refs/heads/jj-hooks-fixup/<name>` ref. Returns the list
/// of bookmarks that were advanced.
pub fn maybe_advance_bookmarks(
    jj: &JjCli,
    report: &PushReport,
    advance_bookmarks: bool,
) -> Result<Vec<String>> {
    if !advance_bookmarks {
        return Ok(vec![]);
    }
    let mut advanced = vec![];
    for (update, outcome) in &report.per_bookmark {
        if let Some(commit) = &outcome.fixup_commit {
            jj.run(&[
                "bookmark",
                "set",
                &update.bookmark,
                "-r",
                commit,
                "--allow-backwards",
            ])?;
            // The temp jj-hooks-fixup bookmark was already forgotten by
            // `hooks::run_for_update` after `jj git import` — nothing to
            // clean up here.
            advanced.push(update.bookmark.clone());
        }
    }
    Ok(advanced)
}

fn dry_run_updates(
    jj: &JjCli,
    push_args: &[String],
) -> Result<std::collections::HashSet<BookmarkUpdate>> {
    let mut args = vec!["git", "push", "--dry-run"];
    let extra: Vec<&str> = push_args.iter().map(|s| s.as_str()).collect();
    args.extend(extra.iter().copied());
    let output = jj.run_capture_stderr(&args)?;
    tracing::debug!("dry-run output:\n{output}");
    parse_git_push_dry_run(&output)
}

/// Run the actual `jj git push` after hooks succeeded.
pub fn execute_push(jj: &JjCli, push_args: &[String], dry_run: bool) -> Result<()> {
    let mut args = vec!["git".to_owned(), "push".to_owned()];
    args.extend(push_args.iter().cloned());
    if dry_run {
        args.push("--dry-run".to_owned());
    }

    let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let status = Command::new("jj")
        .args(&argv)
        .current_dir(jj.cwd())
        .status()?;
    if !status.success() {
        return Err(JjHooksError::JjFailed {
            status: status.code().unwrap_or(-1),
            stderr: "jj git push failed".into(),
        });
    }
    Ok(())
}
