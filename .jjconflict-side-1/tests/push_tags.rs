//! Integration tests for the `push-tags` subcommand.
//!
//! Exercises the end-to-end path: create a jj tag on a commit, invoke
//! `jj-hooks push-tags <tag>`, and assert the bare-git remote received
//! the matching `refs/tags/<tag>` ref.

mod harness;

use harness::{TestRepo, run_jj, show};
use std::process::Command;

/// Create a tag on `@-` (the initial commit) and push it via
/// `jj-hooks push-tags <name>`. Asserts the remote ends up with
/// the tag ref pointing at the same commit.
#[test]
fn push_tags_single_tag_lands_on_remote() {
    let repo = TestRepo::new();
    run_jj(repo.primary(), &["tag", "set", "v0.1.0", "-r", "@-"]);

    let out = repo.jj_hooks(&["push-tags", "v0.1.0"]);
    assert!(out.status.success(), "push-tags failed: {}", show(&out));

    let remote_tag = remote_tag_commit(&repo, "v0.1.0");
    assert!(
        remote_tag.is_some(),
        "remote did not receive refs/tags/v0.1.0"
    );
    let local_at = repo.commit_id_of("@-");
    assert_eq!(remote_tag.unwrap(), local_at);
}

/// `--all` pushes every local tag. Two tags, one invocation, both
/// land on the remote.
#[test]
fn push_tags_all_flag_pushes_every_local_tag() {
    let repo = TestRepo::new();
    run_jj(repo.primary(), &["tag", "set", "v0.1.0", "-r", "@-"]);
    run_jj(repo.primary(), &["tag", "set", "v0.2.0-rc.1", "-r", "@-"]);

    let out = repo.jj_hooks(&["push-tags", "--all"]);
    assert!(
        out.status.success(),
        "push-tags --all failed: {}",
        show(&out)
    );

    assert!(remote_tag_commit(&repo, "v0.1.0").is_some());
    assert!(remote_tag_commit(&repo, "v0.2.0-rc.1").is_some());
}

/// A tag that doesn't exist locally should produce a non-zero exit
/// with an informative error — not a confusing `git push` failure
/// mid-loop.
#[test]
fn push_tags_missing_local_tag_errors_clearly() {
    let repo = TestRepo::new();
    let out = repo.jj_hooks(&["push-tags", "nonexistent"]);
    assert!(
        !out.status.success(),
        "expected failure, got: {}",
        show(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("does not exist locally"),
        "stderr missing 'does not exist locally': {stderr}"
    );
}

/// `--dry-run` prints the planned push without touching the remote.
#[test]
fn push_tags_dry_run_does_not_touch_remote() {
    let repo = TestRepo::new();
    run_jj(repo.primary(), &["tag", "set", "v9.9.9", "-r", "@-"]);

    let out = repo.jj_hooks(&["push-tags", "--dry-run", "v9.9.9"]);
    assert!(out.status.success(), "dry-run failed: {}", show(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("DRY-RUN") && stdout.contains("v9.9.9"),
        "stdout missing dry-run line: {stdout}"
    );
    assert!(
        remote_tag_commit(&repo, "v9.9.9").is_none(),
        "remote should not have received the tag"
    );
}

/// commit id pointed to by a tag ref on the bare remote, or None.
fn remote_tag_commit(repo: &TestRepo, tag: &str) -> Option<String> {
    let out = Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/tags/{tag}"),
        ])
        .current_dir(&repo.remote)
        .output()
        .unwrap();
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}
