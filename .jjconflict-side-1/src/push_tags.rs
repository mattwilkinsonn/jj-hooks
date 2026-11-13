//! `jj-hp push-tags` — push jj tags to a git remote.
//!
//! jj has no native `jj git push --tag`. This subcommand is the
//! workaround: export refs to the colocated git repo, then shell out
//! to `git push refs/tags/<tag>` for each requested tag. Works in
//! both primary and secondary jj workspaces (`git push` discovers
//! the shared `.git` via the standard rules).
//!
//! Ported from `~/scripts/bin/jj-push-tags` during the dotfiles →
//! zireael migration. The shell script is being retired; users get
//! the same behaviour via `jj-hp push-tags …`.

use std::process::Command;

use crate::error::{JjHooksError, Result};
use crate::jj::JjCli;

pub struct PushTagsOpts<'a> {
    pub remote: &'a str,
    pub tags: Vec<String>,
    pub all: bool,
    pub force: bool,
    pub dry_run: bool,
}

pub fn run(jj: &JjCli, opts: PushTagsOpts<'_>) -> Result<()> {
    // Ensure jj has exported all local refs (including tags) into the
    // colocated git ref store. In a colocated repo `jj git export` is a
    // no-op when refs are already in sync, but it's cheap insurance.
    // `--ignore-working-copy` skips the working-copy snapshot, which
    // avoids noisy "Refused to snapshot some files" warnings when
    // ./target or similar build artifacts exceed jj's snapshot limit.
    let _ = jj.run(&["git", "export", "--ignore-working-copy"]);

    let tags = if opts.all {
        list_local_tags(jj)?
    } else {
        opts.tags
    };

    if tags.is_empty() {
        eprintln!("jj-hp push-tags: no tags to push");
        return Ok(());
    }

    for tag in &tags {
        // Refuse to push a tag that doesn't exist locally — early
        // error is friendlier than a `git push` failure mid-loop.
        if !git_tag_exists(jj, tag)? {
            return Err(JjHooksError::JjFailed {
                status: 1,
                stderr: format!("refs/tags/{tag} does not exist locally\n"),
            });
        }

        let mut cmd = Command::new("git");
        cmd.current_dir(jj.cwd()).arg("push");
        if opts.force {
            cmd.arg("--force");
        }
        cmd.arg(opts.remote).arg(format!("refs/tags/{tag}"));

        if opts.dry_run {
            println!(
                "DRY-RUN: git push{} {} refs/tags/{tag}",
                if opts.force { " --force" } else { "" },
                opts.remote
            );
            continue;
        }

        let status = cmd.status()?;
        if !status.success() {
            return Err(JjHooksError::JjFailed {
                status: status.code().unwrap_or(-1),
                stderr: format!("git push refs/tags/{tag} failed\n"),
            });
        }
    }

    Ok(())
}

/// `jj tag list -T 'name ++ "\n"'` — one tag per line. Filters out
/// blanks so an empty list doesn't smuggle an empty tag through.
fn list_local_tags(jj: &JjCli) -> Result<Vec<String>> {
    let out = jj.run(&[
        "--ignore-working-copy",
        "tag",
        "list",
        "-T",
        "name ++ \"\\n\"",
    ])?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect())
}

/// `git rev-parse --verify --quiet refs/tags/<tag>` — exit 0 if the
/// tag exists, non-zero otherwise. We don't care about the resolved
/// SHA, just the existence check.
fn git_tag_exists(jj: &JjCli, tag: &str) -> Result<bool> {
    let status = Command::new("git")
        .current_dir(jj.cwd())
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/tags/{tag}"),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    Ok(status.success())
}
