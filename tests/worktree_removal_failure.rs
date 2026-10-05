#![cfg(unix)]

//! Exercise dead-owner cleanup with a Git failure isolated to the `jj-hp` process.

mod harness;

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use harness::{TestRepo, show};

fn advance_main(repo: &TestRepo) {
    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "advance"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));
}

fn git_dir(repo: &TestRepo) -> PathBuf {
    let out = Command::new("git")
        .args(["rev-parse", "--absolute-git-dir"])
        .current_dir(repo.primary())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", show(&out));
    PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
}

fn listed_worktree_count(repo: &TestRepo) -> usize {
    let out = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo.primary())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", show(&out));
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count()
}

fn dead_owner_worktree(repo: &TestRepo, root: &Path) -> (PathBuf, PathBuf) {
    let git_dir = git_dir(repo);
    let checkout = root.join("jj-hooks-worktree-dead-owner");
    let out = Command::new("git")
        .args(["worktree", "add", "--detach", "--quiet"])
        .arg(&checkout)
        .arg("HEAD")
        .current_dir(repo.primary())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", show(&out));

    let mut owner_path = OsString::from(checkout.as_os_str());
    owner_path.push(".lock");
    let owner_path = PathBuf::from(owner_path);
    std::fs::write(&owner_path, format!("1\n{}\n", git_dir.display())).unwrap();
    (checkout, owner_path)
}

#[test]
fn failed_git_removal_preserves_dead_owner_worktree_and_lock() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(
        "repos:\n  - repo: local\n    hooks:\n      - id: pass\n        name: pass\n        entry: 'true'\n        language: system\n        stages: [pre-push]\n        always_run: true\n        pass_filenames: false\n",
    );
    advance_main(&repo);

    let root = repo.tmp.path().join("worktrees");
    std::fs::create_dir(&root).unwrap();
    let (checkout, owner_path) = dead_owner_worktree(&repo, &root);
    assert_eq!(listed_worktree_count(&repo), 2);

    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("git"))
        .find(|path| path.is_file())
        .expect("git must be available on PATH");
    let shim_dir = repo.tmp.path().join("git-shim");
    std::fs::create_dir(&shim_dir).unwrap();
    let shim = shim_dir.join("git");
    std::fs::write(
        &shim,
        "#!/bin/sh\nif [ \"$2\" = worktree ] && [ \"$3\" = remove ] && [ \"$6\" = \"$JJ_HOOKS_TEST_FAIL_CHECKOUT\" ]; then\n  : > \"$JJ_HOOKS_TEST_GIT_REMOVE_OUT\"\n  echo 'simulated Git removal failure' >&2\n  exit 1\nfi\nexec \"$JJ_HOOKS_TEST_REAL_GIT\" \"$@\"\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&shim, permissions).unwrap();

    let original_path = std::env::var_os("PATH").unwrap();
    let mut path_entries = vec![shim_dir.clone()];
    path_entries.extend(std::env::split_paths(&original_path));
    let path = std::env::join_paths(path_entries).unwrap();
    let removal_attempt = repo.tmp.path().join("git-remove-attempted");
    let output = Command::new(env!("CARGO_BIN_EXE_jj-hp"))
        .args(["--runner", "pre-commit", "push", "-b", "main"])
        .current_dir(repo.primary())
        .env("PATH", path)
        .env("PRE_COMMIT_HOME", &repo.pre_commit_home)
        .env("JJ_HOOKS_LOG", "info")
        .env("JJ_HOOKS_WORKTREE_ROOT", &root)
        .env("JJ_HOOKS_TEST_REAL_GIT", &real_git)
        .env("JJ_HOOKS_TEST_GIT_REMOVE_OUT", &removal_attempt)
        .env("JJ_HOOKS_TEST_FAIL_CHECKOUT", &checkout)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", show(&output));

    assert!(removal_attempt.exists(), "Git remove shim was not invoked");
    assert!(checkout.is_dir(), "dead-owner checkout was removed");
    assert!(owner_path.is_file(), "dead-owner lock was removed");
    assert_eq!(listed_worktree_count(&repo), 2);
}
