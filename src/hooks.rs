//! Per-bookmark hook execution pipeline.
//!
//! For each bookmark update being pushed:
//! 1. Resolve one or more `from_ref` commits (the ancestors on the remote).
//! 2. Create an ephemeral detached worktree at the new commit.
//! 3. Run the configured hook backend against each `from_ref` in turn.
//!    Modifications accumulate in the same worktree.
//! 4. If the worktree ended up with modifications, build a fixup commit
//!    via `git commit-tree`, anchor it under `refs/jj-hooks/fixup/<bookmark>`,
//!    and `jj git import` so jj sees it.
//! 5. Optionally advance the bookmark to the fixup commit.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bookmark_updates::BookmarkUpdate;
use crate::error::{JjHooksError, Result};
use crate::jj::JjCli;
use crate::runner::{Runner, Stage, hook_command, lefthook_command};
use crate::worktree::Worktree;

#[derive(Debug, Clone)]
pub struct HookOutcome {
    /// All hook runs for this bookmark exited 0.
    pub success: bool,
    /// Commit id of the fixup commit if the hook(s) modified files.
    pub fixup_commit: Option<String>,
}

/// Run hooks for one bookmark update. Returns the outcome (success +
/// optional fixup commit).
pub fn run_for_update(
    jj: &JjCli,
    primary_git_dir: &Path,
    runner: Runner,
    stage: Stage,
    update: &BookmarkUpdate,
) -> Result<HookOutcome> {
    let Some(new_commit) = update.new_commit.as_ref() else {
        // Pure delete — nothing to check.
        return Ok(HookOutcome {
            success: true,
            fixup_commit: None,
        });
    };

    let from_refs = resolve_from_refs(jj, update)?;

    let wt = Worktree::create(primary_git_dir, new_commit)?;

    let mut success = true;
    for from_ref in &from_refs {
        let argv = match runner {
            Runner::Lefthook => {
                let files = changed_files(wt.path(), from_ref, new_commit)?;
                lefthook_command(stage, &files)
            }
            _ => hook_command(runner, stage, from_ref, new_commit),
        };

        tracing::info!("running: {:?}", argv);
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(wt.path())
            .status()?;

        if !status.success() {
            success = false;
        }
    }

    let fixup_commit = if worktree_dirty(wt.path())? {
        Some(build_fixup_commit(
            primary_git_dir,
            wt.path(),
            new_commit,
            &update.bookmark,
        )?)
    } else {
        None
    };

    if fixup_commit.is_some() {
        // Make jj aware of the new commit.
        jj.run(&["git", "import"])?;

        // jj git import created a `jj-hooks-fixup/<bookmark>` jj bookmark
        // from the underlying refs/heads/jj-hooks-fixup/<bookmark> ref.
        // Clean both up immediately — the user almost always wants to
        // either squash the fixup into the parent or move their bookmark
        // forward themselves, not have a stale temp bookmark lying
        // around. The commit stays addressable by hash via `jj log`,
        // `jj show`, `jj squash --from <hash>` etc. since jj tracks it
        // in its own commit graph independent of the ref.
        let temp_bookmark = fixup_bookmark(&update.bookmark);
        // `jj bookmark forget` removes the jj bookmark, but in a
        // secondary workspace it leaves the underlying refs/heads/<name>
        // ref alive in the primary's git dir. Explicitly delete the
        // git ref ourselves so the cleanup is uniform.
        let _ = jj.run(&["bookmark", "forget", &temp_bookmark]);
        let _ = delete_git_ref(primary_git_dir, &fixup_ref(&update.bookmark));
    }

    Ok(HookOutcome {
        success,
        fixup_commit,
    })
}

/// Resolve the `from_ref` commits to diff against. For an existing
/// bookmark update we just use the old commit; for a new bookmark we
/// find the heads of `::new & ::remote_bookmarks(remote)` so each
/// already-on-remote ancestor becomes its own diff base.
fn resolve_from_refs(jj: &JjCli, update: &BookmarkUpdate) -> Result<Vec<String>> {
    if let Some(old) = update.old_commit.as_ref() {
        return Ok(vec![old.clone()]);
    }

    let new = update.new_commit.as_ref().expect("not a delete here");
    let revset = format!(
        "heads(::{new} & ::remote_bookmarks(remote=exact:{}))",
        update.remote
    );

    let template = r#"commit_id ++ "\n""#;
    let out = jj.run(&[
        "log",
        "--no-graph",
        "-r",
        &revset,
        "-T",
        template,
        "--ignore-working-copy",
    ])?;

    let refs: Vec<String> = out
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect();

    if refs.is_empty() {
        // New bookmark on a totally fresh remote — no ancestors on the
        // remote at all. Use the parent of new as the diff base.
        return Ok(vec![format!("{new}^")]);
    }

    Ok(refs)
}

fn changed_files(worktree: &Path, from: &str, to: &str) -> Result<Vec<PathBuf>> {
    let out = Command::new("git")
        .args(["diff", "--name-only", "--diff-filter=ACMR"])
        .arg(format!("{from}..{to}"))
        .current_dir(worktree)
        .output()?;
    if !out.status.success() {
        return Err(JjHooksError::JjFailed {
            status: out.status.code().unwrap_or(-1),
            stderr: format!(
                "git diff --name-only failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| PathBuf::from(l.trim()))
        .filter(|p| !p.as_os_str().is_empty())
        .collect())
}

fn worktree_dirty(worktree: &Path) -> Result<bool> {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(worktree)
        .output()?;
    Ok(!out.stdout.is_empty())
}

fn build_fixup_commit(
    primary_git_dir: &Path,
    worktree: &Path,
    parent: &str,
    bookmark: &str,
) -> Result<String> {
    // Stage everything (tracked + untracked) in the worktree.
    run_git(worktree, &["add", "-A"])?;

    // Write the tree.
    let tree = run_git_capture(worktree, &["write-tree"])?;

    // Build the commit object via the *primary* git dir so the resulting
    // commit lives in the shared object database.
    let message = format!("jj-hooks: autofixes for {bookmark}");
    let commit = run_git_capture_with_git_dir(
        primary_git_dir,
        worktree,
        &["commit-tree", &tree, "-p", parent, "-m", &message],
    )?;

    // Anchor under refs/heads/ so `jj git import` will pick it up as a
    // bookmark. (Refs outside refs/heads/ and refs/remotes/ are invisible
    // to jj's git import logic.)
    let ref_name = fixup_ref(bookmark);
    run_git_capture_with_git_dir(
        primary_git_dir,
        worktree,
        &["update-ref", &ref_name, &commit],
    )?;

    Ok(commit)
}

/// The git ref where a fixup commit gets anchored for a given bookmark.
/// Lives under `refs/heads/` so `jj git import` picks it up as a bookmark.
pub fn fixup_ref(bookmark: &str) -> String {
    format!("refs/heads/jj-hooks-fixup/{}", sanitize_for_ref(bookmark))
}

/// The jj bookmark name corresponding to `fixup_ref`.
pub fn fixup_bookmark(bookmark: &str) -> String {
    format!("jj-hooks-fixup/{}", sanitize_for_ref(bookmark))
}

/// Replace characters that git rejects in ref names (per git-check-ref-format)
/// with `_`. Real bookmark names like `main` or `feature/foo` pass through
/// unchanged; synthesized names like `revset:@` (used by `jj-hp run @`) get
/// scrubbed so the resulting `refs/heads/jj-hooks-fixup/<name>` is valid.
fn sanitize_for_ref(s: &str) -> String {
    // Per-character offenders first; then collapse multi-char sequences
    // and trim the position-sensitive ones (leading `-`/`.`, trailing
    // `.`/`.lock`/`/`, internal `//`).
    let mut out: String = s
        .chars()
        .map(|c| match c {
            ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\' | '\x7f' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();

    while out.contains("..") {
        out = out.replace("..", "__");
    }
    while out.contains("@{") {
        out = out.replace("@{", "@_");
    }
    if out.starts_with('-') {
        out.replace_range(0..1, "_");
    }
    if out.starts_with('.') {
        out.replace_range(0..1, "_");
    }
    if out.ends_with('.') {
        let n = out.len();
        out.replace_range(n - 1..n, "_");
    }
    if out.ends_with(".lock") {
        let n = out.len();
        out.replace_range(n - 5..n - 4, "_");
    }
    if out.ends_with('/') {
        let n = out.len();
        out.replace_range(n - 1..n, "_");
    }
    while out.contains("//") {
        out = out.replace("//", "/_");
    }
    if out.is_empty() {
        return "_".into();
    }
    out
}

/// Delete a git ref in the given git dir, ignoring "ref doesn't exist"
/// failures. Used to clean up the temp `refs/heads/jj-hooks-fixup/<name>`
/// after `jj git import` + `jj bookmark forget` from a secondary
/// workspace (where forget leaves the underlying ref alive).
fn delete_git_ref(git_dir: &Path, ref_name: &str) -> Result<()> {
    let out = Command::new("git")
        .arg(format!("--git-dir={}", git_dir.display()))
        .args(["update-ref", "-d", ref_name])
        .output()?;
    if !out.status.success() {
        // Treat any failure as best-effort: if the ref didn't exist,
        // that's the desired state already.
        tracing::debug!(
            "git update-ref -d {ref_name} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git").args(args).current_dir(cwd).output()?;
    if !out.status.success() {
        return Err(JjHooksError::JjFailed {
            status: out.status.code().unwrap_or(-1),
            stderr: format!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        });
    }
    Ok(())
}

fn run_git_capture(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").args(args).current_dir(cwd).output()?;
    if !out.status.success() {
        return Err(JjHooksError::JjFailed {
            status: out.status.code().unwrap_or(-1),
            stderr: format!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn run_git_capture_with_git_dir(git_dir: &Path, cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg(format!("--git-dir={}", git_dir.display()))
        .args(args)
        .current_dir(cwd)
        .output()?;
    if !out.status.success() {
        return Err(JjHooksError::JjFailed {
            status: out.status.code().unwrap_or(-1),
            stderr: format!(
                "git --git-dir={} {args:?} failed: {}",
                git_dir.display(),
                String::from_utf8_lossy(&out.stderr)
            ),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixup_ref_for_plain_bookmark() {
        assert_eq!(fixup_ref("main"), "refs/heads/jj-hooks-fixup/main");
    }

    #[test]
    fn fixup_ref_keeps_internal_slash() {
        // jj bookmark names commonly contain `/` (e.g. `feature/foo`) and
        // git accepts them as path separators inside a ref.
        assert_eq!(
            fixup_ref("feature/foo"),
            "refs/heads/jj-hooks-fixup/feature/foo"
        );
    }

    #[test]
    fn fixup_ref_scrubs_colon() {
        // The bug from issue #1: `jj-hp run @` synthesizes `revset:@`.
        // Without sanitization, git rejects the ref with "bad name".
        assert_eq!(fixup_ref("revset:@"), "refs/heads/jj-hooks-fixup/revset_@");
    }

    #[test]
    fn sanitize_replaces_each_invalid_char() {
        // One probe per character class git-check-ref-format rejects.
        assert_eq!(sanitize_for_ref("a:b"), "a_b");
        assert_eq!(sanitize_for_ref("a~b"), "a_b");
        assert_eq!(sanitize_for_ref("a^b"), "a_b");
        assert_eq!(sanitize_for_ref("a?b"), "a_b");
        assert_eq!(sanitize_for_ref("a*b"), "a_b");
        assert_eq!(sanitize_for_ref("a[b"), "a_b");
        assert_eq!(sanitize_for_ref("a\\b"), "a_b");
        assert_eq!(sanitize_for_ref("a b"), "a_b");
        assert_eq!(sanitize_for_ref("a\tb"), "a_b");
        assert_eq!(sanitize_for_ref("a\x7fb"), "a_b");
    }

    #[test]
    fn sanitize_collapses_double_dot() {
        assert_eq!(sanitize_for_ref("a..b"), "a__b");
        // `..` replacement is non-overlapping: `a...b` becomes `a__.b`
        // (first `..` matches at positions 1-2 and gets replaced; the
        // remaining `.` is harmless mid-string).
        assert_eq!(sanitize_for_ref("a...b"), "a__.b");
        assert!(!sanitize_for_ref("a....b").contains(".."));
    }

    #[test]
    fn sanitize_collapses_at_brace() {
        assert_eq!(sanitize_for_ref("a@{b"), "a@_b");
    }

    #[test]
    fn sanitize_strips_leading_dash() {
        assert_eq!(sanitize_for_ref("-foo"), "_foo");
    }

    #[test]
    fn sanitize_strips_leading_dot() {
        assert_eq!(sanitize_for_ref(".foo"), "_foo");
    }

    #[test]
    fn sanitize_strips_trailing_dot() {
        assert_eq!(sanitize_for_ref("foo."), "foo_");
    }

    #[test]
    fn sanitize_strips_trailing_dot_lock() {
        assert_eq!(sanitize_for_ref("foo.lock"), "foo_lock");
    }

    #[test]
    fn sanitize_strips_trailing_slash() {
        assert_eq!(sanitize_for_ref("foo/"), "foo_");
    }

    #[test]
    fn sanitize_collapses_double_slash() {
        assert_eq!(sanitize_for_ref("a//b"), "a/_b");
    }

    #[test]
    fn sanitize_empty_becomes_underscore() {
        // Defensive: if the input is empty after some external transform,
        // emit a single underscore so the joined ref isn't dangling.
        assert_eq!(sanitize_for_ref(""), "_");
    }

    #[test]
    fn fixup_bookmark_uses_same_sanitizer() {
        // fixup_bookmark feeds `jj bookmark forget` which is also strict
        // about colon (jj rejects bookmark names with `:` in them).
        assert_eq!(fixup_bookmark("revset:@"), "jj-hooks-fixup/revset_@");
    }
}
