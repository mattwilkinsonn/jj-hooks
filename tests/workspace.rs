//! Integration tests against real jj + git repos in tempdirs.
//!
//! Requires `jj` and `git` on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;

use jj_hooks::jj::{self, JjCli};
use jj_hooks::worktree::Worktree;
use tempfile::TempDir;

/// Set up a primary colocated jj+git repo at `<tmp>/primary` with one commit.
/// Returns (tmp, primary_path, head_commit_id).
fn setup_primary_repo() -> (TempDir, PathBuf, String) {
    let tmp = TempDir::new().unwrap();
    let primary = tmp.path().join("primary");
    std::fs::create_dir(&primary).unwrap();

    run(&primary, "jj", &["git", "init", "--colocate"]);

    std::fs::write(primary.join("hello.txt"), "hello\n").unwrap();
    run(&primary, "jj", &["commit", "-m", "initial"]);

    // commit_id of @- (the change we just committed)
    let commit_id = run_capture(
        &primary,
        "jj",
        &[
            "log",
            "--no-graph",
            "-r",
            "@-",
            "-T",
            "commit_id",
            "--color",
            "never",
            "--ignore-working-copy",
        ],
    );

    (tmp, primary, commit_id)
}

#[test]
fn primary_git_dir_on_primary_workspace() {
    let (_tmp, primary, _) = setup_primary_repo();
    let resolved = jj::primary_git_dir(&primary).unwrap();
    let expected = primary.join(".git").canonicalize().unwrap();
    assert_eq!(resolved, expected);
}

#[test]
fn primary_git_dir_on_secondary_workspace() {
    let (_tmp, primary, _) = setup_primary_repo();

    let secondary = primary.parent().unwrap().join("secondary");
    run(
        &primary,
        "jj",
        &["workspace", "add", secondary.to_str().unwrap(), "-r", "@-"],
    );

    let from_primary = jj::primary_git_dir(&primary).unwrap();
    let from_secondary = jj::primary_git_dir(&secondary).unwrap();
    assert_eq!(from_primary, from_secondary);
    assert_eq!(from_primary, primary.join(".git").canonicalize().unwrap());
}

#[test]
fn workspace_root_resolves_from_subdirectory() {
    let (_tmp, primary, _) = setup_primary_repo();
    let sub = primary.join("nested/dir");
    std::fs::create_dir_all(&sub).unwrap();

    let jj_cli = JjCli::new(&sub);
    let root = jj_cli.workspace_root().unwrap();
    assert_eq!(root, primary.canonicalize().unwrap());
}

#[test]
fn worktree_creates_then_removes_on_drop() {
    let (_tmp, primary, commit_id) = setup_primary_repo();
    let git_dir = jj::primary_git_dir(&primary).unwrap();

    let path: PathBuf;
    {
        let wt = Worktree::create(&git_dir, &commit_id).unwrap();
        path = wt.path().to_owned();
        assert!(path.exists(), "worktree dir should exist while guard alive");
        assert!(
            path.join("hello.txt").exists(),
            "target commit's file should be checked out"
        );
    }
    assert!(
        !path.exists(),
        "worktree dir should be removed when guard drops"
    );

    // And git should agree it's gone.
    let list = run_capture(&primary, "git", &["worktree", "list", "--porcelain"]);
    assert!(
        !list.contains(path.to_str().unwrap()),
        "git worktree list should not include the removed worktree:\n{list}"
    );
}

#[test]
fn worktree_in_secondary_workspace_uses_primary_git_dir() {
    let (_tmp, primary, commit_id) = setup_primary_repo();

    let secondary = primary.parent().unwrap().join("secondary");
    run(
        &primary,
        "jj",
        &["workspace", "add", secondary.to_str().unwrap(), "-r", "@-"],
    );

    let git_dir = jj::primary_git_dir(&secondary).unwrap();
    let wt = Worktree::create(&git_dir, &commit_id).unwrap();
    assert!(wt.path().join("hello.txt").exists());
}

fn run(cwd: &Path, prog: &str, args: &[&str]) {
    let out = Command::new(prog)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn {prog}: {e}"));
    if !out.status.success() {
        panic!(
            "{prog} {args:?} failed in {cwd:?}:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

fn run_capture(cwd: &Path, prog: &str, args: &[&str]) -> String {
    let out = Command::new(prog)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn {prog}: {e}"));
    if !out.status.success() {
        panic!(
            "{prog} {args:?} failed in {cwd:?}:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}
