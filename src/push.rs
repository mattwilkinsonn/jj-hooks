//! Push pipeline: dry-run parse → per-bookmark hook → push or abort.

use std::path::Path;
use std::process::Command;

use crate::bookmark_updates::{BookmarkUpdate, UpdateType, parse_git_push_dry_run};
use crate::error::{JjHooksError, Result};
use crate::hooks::{HookOutcome, RunOpts, run_for_update};
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
    run_opts: RunOpts,
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
        let outcome = run_for_update(
            jj,
            &primary_git_dir,
            workspace_root,
            cli_runner,
            stage,
            &update,
            run_opts,
        )?;
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
            let argv = advance_bookmark_argv(&update.bookmark, commit);
            let argv: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
            jj.run(&argv)?;
            // The temp jj-hooks-fixup bookmark was already forgotten by
            // `hooks::run_for_update` after `jj git import` — nothing to
            // clean up here.
            advanced.push(update.bookmark.clone());
        }
    }
    Ok(advanced)
}

/// Build the argv for `jj bookmark set` to advance a local bookmark to its
/// fixup commit. `--ignore-working-copy` keeps this from snapshotting the
/// user's working copy and racing against any other `jj` process they might
/// be running in parallel — bookmark targets come from `commit` (the fixup
/// hash), not from the working copy state.
pub(crate) fn advance_bookmark_argv(bookmark: &str, commit: &str) -> Vec<String> {
    vec![
        "bookmark".into(),
        "set".into(),
        bookmark.into(),
        "-r".into(),
        commit.into(),
        "--allow-backwards".into(),
        "--ignore-working-copy".into(),
    ]
}

fn dry_run_updates(
    jj: &JjCli,
    push_args: &[String],
) -> Result<std::collections::HashSet<BookmarkUpdate>> {
    let args = dry_run_argv(push_args);
    let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = jj.run_capture_stderr(&argv)?;
    tracing::debug!("dry-run output:\n{output}");
    parse_git_push_dry_run(&output)
}

/// Build the argv for the `jj git push --dry-run` probe that resolves which
/// bookmarks would be updated. `--ignore-working-copy` avoids contending for
/// the op lock with any concurrent `jj` invocation — the probe only reads
/// bookmark state, not the user's working copy.
pub(crate) fn dry_run_argv(push_args: &[String]) -> Vec<String> {
    let mut args = vec![
        "git".into(),
        "push".into(),
        "--dry-run".into(),
        "--ignore-working-copy".into(),
    ];
    args.extend(push_args.iter().cloned());
    args
}

/// Run the actual `jj git push` after hooks succeeded.
pub fn execute_push(jj: &JjCli, push_args: &[String], dry_run: bool) -> Result<()> {
    let args = execute_push_argv(push_args, dry_run);
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

/// Build the argv for the final `jj git push`. `--ignore-working-copy` is
/// what makes `jj-hp push` safe to run while the user is doing unrelated
/// `jj` work in another shell: hooks have already run against the target
/// commits in an ephemeral worktree, so the push doesn't need the user's
/// working copy. Without this flag, jj snapshots/updates the working copy
/// around the push and bails with "Concurrent checkout" when another `jj`
/// process held the op lock.
pub(crate) fn execute_push_argv(push_args: &[String], dry_run: bool) -> Vec<String> {
    let mut args = vec!["git".into(), "push".into(), "--ignore-working-copy".into()];
    args.extend(push_args.iter().cloned());
    if dry_run {
        args.push("--dry-run".into());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_push_argv_includes_ignore_working_copy() {
        // Regression for issue #3: `jj git push` must run with
        // `--ignore-working-copy` so it doesn't fight for the op lock with
        // a concurrent `jj` process (`jj new`, `jj edit`, etc.) in another
        // shell. Without this, the push aborts with "Concurrent checkout".
        let argv = execute_push_argv(&["-b".into(), "main".into()], false);
        assert!(
            argv.iter().any(|a| a == "--ignore-working-copy"),
            "execute_push must pass --ignore-working-copy: {argv:?}"
        );
        assert_eq!(argv[0], "git");
        assert_eq!(argv[1], "push");
    }

    #[test]
    fn execute_push_argv_appends_dry_run_when_set() {
        let argv = execute_push_argv(&[], true);
        assert!(argv.iter().any(|a| a == "--dry-run"));
        assert!(argv.iter().any(|a| a == "--ignore-working-copy"));
    }

    #[test]
    fn execute_push_argv_passes_through_caller_args() {
        let argv = execute_push_argv(
            &[
                "-b".into(),
                "feature".into(),
                "--remote".into(),
                "origin".into(),
            ],
            false,
        );
        for needle in ["-b", "feature", "--remote", "origin"] {
            assert!(
                argv.iter().any(|a| a == needle),
                "expected `{needle}` in argv: {argv:?}"
            );
        }
    }

    #[test]
    fn dry_run_argv_includes_ignore_working_copy() {
        // The dry-run probe runs first and is the most common race victim:
        // the bookmark-resolution step would fail before hooks ever start.
        let argv = dry_run_argv(&["-b".into(), "main".into()]);
        assert!(
            argv.iter().any(|a| a == "--ignore-working-copy"),
            "dry-run argv must include --ignore-working-copy: {argv:?}"
        );
        assert!(argv.iter().any(|a| a == "--dry-run"));
    }

    #[test]
    fn advance_bookmark_argv_includes_ignore_working_copy() {
        // Post-autofix `jj bookmark set` runs after hooks completed — the
        // user may already have moved to another change, so this is the
        // most likely site for a working-copy snapshot race.
        let argv = advance_bookmark_argv("main", "deadbeef");
        assert!(
            argv.iter().any(|a| a == "--ignore-working-copy"),
            "advance-bookmark argv must include --ignore-working-copy: {argv:?}"
        );
        assert_eq!(argv[0], "bookmark");
        assert_eq!(argv[1], "set");
        assert_eq!(argv[2], "main");
        assert!(argv.iter().any(|a| a == "--allow-backwards"));
    }
}
