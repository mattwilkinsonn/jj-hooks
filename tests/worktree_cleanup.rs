//! Regression tests for worktree placement and leak cleanup.

mod harness;

use std::path::{Path, PathBuf};
use std::process::Command;

use harness::{TestRepo, show};

const PRE_PUSH_RECORD_CWD: &str = r#"
repos:
  - repo: local
    hooks:
      - id: record-cwd
        name: record-cwd
        entry: sh -c 'printf "%s" "$PWD" > "$JJ_HOOKS_TEST_CWD_OUT"'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;

const PRE_PUSH_RECORD_CWD_AND_FAIL: &str = r#"
repos:
  - repo: local
    hooks:
      - id: record-cwd
        name: record-cwd
        entry: sh -c 'printf "%s" "$PWD" > "$JJ_HOOKS_TEST_CWD_OUT"'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
      - id: fail
        name: fail
        entry: 'false'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;

fn advance_main(repo: &TestRepo) {
    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "advance"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));
}

fn push_with_root(repo: &TestRepo, root: &Path, cwd_out: &Path) -> std::process::Output {
    let root = root.to_str().unwrap();
    let cwd_out = cwd_out.to_str().unwrap();
    repo.jj_hooks_with_env(
        &["--runner", "pre-commit", "push", "-b", "main"],
        &[
            ("JJ_HOOKS_WORKTREE_ROOT", root),
            ("JJ_HOOKS_TEST_CWD_OUT", cwd_out),
        ],
    )
}

fn assert_only_primary_worktree(repo: &TestRepo) {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo.primary())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", show(&output));
    let listing = String::from_utf8(output.stdout).unwrap();
    let paths: Vec<_> = listing
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .collect();
    assert_eq!(paths, [repo.primary().to_str().unwrap()], "{listing}");
}

fn assert_worktree_was_cleaned(repo: &TestRepo, root: &Path, cwd_out: &Path) {
    let cwd = std::fs::read_to_string(cwd_out).unwrap();
    assert!(
        cwd.starts_with(root.to_str().unwrap()),
        "hook cwd was {cwd:?}"
    );
    assert!(
        !PathBuf::from(&cwd).exists(),
        "worktree still exists at {cwd}"
    );
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
    assert_only_primary_worktree(repo);
}

#[test]
fn passing_hook_uses_configured_worktree_root_and_cleans_it() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_RECORD_CWD);
    advance_main(&repo);

    let root = repo.tmp.path().join("worktrees");
    std::fs::create_dir(&root).unwrap();
    let cwd_out = repo.tmp.path().join("hook-cwd");
    let out = push_with_root(&repo, &root, &cwd_out);
    assert!(out.status.success(), "{}", show(&out));

    assert_worktree_was_cleaned(&repo, &root, &cwd_out);
}

#[test]
fn failing_hook_cleans_configured_worktree_root() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_RECORD_CWD_AND_FAIL);
    advance_main(&repo);

    let root = repo.tmp.path().join("worktrees");
    std::fs::create_dir(&root).unwrap();
    let cwd_out = repo.tmp.path().join("hook-cwd");
    let out = push_with_root(&repo, &root, &cwd_out);
    assert!(
        !out.status.success(),
        "expected the failing hook to abort the push: {}",
        show(&out)
    );

    assert_worktree_was_cleaned(&repo, &root, &cwd_out);
}

#[cfg(target_os = "linux")]
fn file_has_content(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|contents| !contents.is_empty())
}

#[cfg(target_os = "linux")]
const PRE_PUSH_SLEEPER: &str = r#"
repos:
  - repo: local
    hooks:
      - id: sleeper
        name: sleeper
        entry: sh -c 'set -- $(cat /proc/$$/stat); printf "%s" "$PWD" > "$JJ_HOOKS_TEST_CWD_OUT"; printf "%s" "$5" > "$JJ_HOOKS_TEST_PGRP_OUT"; (while :; do date > "$JJ_HOOKS_TEST_TICK_OUT"; sleep 0.1; done) & echo "$!" > "$JJ_HOOKS_TEST_PID_OUT"; wait'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;

#[cfg(target_os = "linux")]
#[test]
fn next_push_sweeps_worktree_after_owner_is_killed() {
    use std::process::Command;
    use std::time::{Duration, Instant};

    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_SLEEPER);
    advance_main(&repo);

    let root = repo.tmp.path().join("worktrees");
    std::fs::create_dir(&root).unwrap();
    let cwd_out = repo.tmp.path().join("hook-cwd");
    let pgrp_out = repo.tmp.path().join("hook-pgrp");
    let pid_out = repo.tmp.path().join("sleeper-pid");
    let tick_out = repo.tmp.path().join("sleeper-tick");
    let log = repo.tmp.path().join("jj-hp.log");
    let root_env = root.to_str().unwrap();
    let cwd_env = cwd_out.to_str().unwrap();
    let pgrp_env = pgrp_out.to_str().unwrap();
    let pid_env = pid_out.to_str().unwrap();
    let tick_env = tick_out.to_str().unwrap();
    let args = ["--runner", "pre-commit", "push", "-b", "main"];
    let mut child = repo.jj_hooks_spawn_with_env(
        &args,
        &[
            ("JJ_HOOKS_WORKTREE_ROOT", root_env),
            ("JJ_HOOKS_TEST_CWD_OUT", cwd_env),
            ("JJ_HOOKS_TEST_PGRP_OUT", pgrp_env),
            ("JJ_HOOKS_TEST_PID_OUT", pid_env),
            ("JJ_HOOKS_TEST_TICK_OUT", tick_env),
        ],
        &log,
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while (!file_has_content(&cwd_out)
        || !file_has_content(&pgrp_out)
        || !file_has_content(&pid_out))
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        cwd_out.exists() && pid_out.exists(),
        "sleeper did not start; log: {}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    let cwd = std::fs::read_to_string(&cwd_out).unwrap();
    let pgrp = std::fs::read_to_string(&pgrp_out).unwrap();
    assert!(
        cwd.starts_with(root.to_str().unwrap()),
        "sleeper cwd was {cwd:?}"
    );
    assert_eq!(
        pgrp.trim(),
        child.id().to_string(),
        "jj-hp did not lead its session"
    );
    let sleeper_pid = std::fs::read_to_string(&pid_out).unwrap();

    child.kill().unwrap();
    child.wait().unwrap();
    let killed = Command::new("kill")
        .args(["-9", sleeper_pid.trim()])
        .status()
        .unwrap();
    assert!(
        killed.success(),
        "failed to kill sleeper pid {}",
        sleeper_pid.trim()
    );

    // Hooks read the config from the pushed commit, so the swap must be committed.
    repo.write_pre_commit_config(PRE_PUSH_RECORD_CWD);
    let out = repo.jj(&["commit", "-m", "record-cwd hook"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = push_with_root(&repo, &root, &cwd_out);
    assert!(out.status.success(), "{}", show(&out));
    assert_worktree_was_cleaned(&repo, &root, &cwd_out);
}
