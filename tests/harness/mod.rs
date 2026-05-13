//! Shared harness for push-pipeline integration tests.
//!
//! Each test gets a fresh tempdir containing:
//! - `<tmp>/remote.git`  — bare git repo serving as `origin`.
//! - `<tmp>/primary`     — colocated jj+git working copy of that remote.
//!
//! pre-commit cache is scoped to `<tmp>/pre-commit-home` via `PRE_COMMIT_HOME`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

pub struct TestRepo {
    pub tmp: TempDir,
    pub primary: PathBuf,
    pub remote: PathBuf,
    pub pre_commit_home: PathBuf,
}

impl TestRepo {
    pub fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let remote = tmp.path().join("remote.git");
        let primary = tmp.path().join("primary");
        let pre_commit_home = tmp.path().join("pre-commit-home");
        std::fs::create_dir(&pre_commit_home).unwrap();

        run(
            tmp.path(),
            "git",
            &["init", "--bare", "--quiet", "remote.git"],
        );

        std::fs::create_dir(&primary).unwrap();
        run_jj(&primary, &["git", "init", "--colocate"]);

        // Pin a deterministic identity inside the repo's config. CI runners
        // don't have user.name/user.email set, and jj refuses to push
        // commits with no author. Setting these via `jj config set --repo`
        // keeps the test hermetic regardless of host state.
        run_jj(
            &primary,
            &["config", "set", "--repo", "user.name", "jj-hooks tests"],
        );
        run_jj(
            &primary,
            &[
                "config",
                "set",
                "--repo",
                "user.email",
                "tests@jj-hooks.invalid",
            ],
        );

        // Same for git's local config — `hooks.rs` shells out to
        // `git commit-tree` to build fixup commits, and commit-tree
        // requires committer/author identity. Set it locally (not
        // --global) so we don't pollute the host machine.
        run(
            &primary,
            "git",
            &["config", "--local", "user.name", "jj-hooks tests"],
        );
        run(
            &primary,
            "git",
            &["config", "--local", "user.email", "tests@jj-hooks.invalid"],
        );

        // First commit so we have something to push.
        std::fs::write(primary.join("README"), "init\n").unwrap();
        run_jj(&primary, &["commit", "-m", "initial"]);

        // Add origin so jj git push has a target.
        run(
            &primary,
            "git",
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );

        // Create main bookmark on the initial commit and push it.
        run_jj(&primary, &["bookmark", "create", "main", "-r", "@-"]);
        run_jj(&primary, &["git", "push", "-b", "main", "--allow-new"]);

        Self {
            tmp,
            primary,
            remote,
            pre_commit_home,
        }
    }

    pub fn primary(&self) -> &Path {
        &self.primary
    }

    pub fn write(&self, rel: &str, content: &str) {
        let p = self.primary.join(rel);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }

    pub fn write_pre_commit_config(&self, yaml: &str) {
        std::fs::write(self.primary.join(".pre-commit-config.yaml"), yaml).unwrap();
    }

    pub fn jj(&self, args: &[&str]) -> Output {
        capture_jj(&self.primary, args)
    }

    pub fn jj_in(&self, cwd: &Path, args: &[&str]) -> Output {
        capture_jj(cwd, args)
    }

    pub fn jj_hooks(&self, args: &[&str]) -> Output {
        self.jj_hooks_in(&self.primary, args)
    }

    pub fn jj_hooks_in(&self, cwd: &Path, args: &[&str]) -> Output {
        let bin = env!("CARGO_BIN_EXE_jj-hooks");
        Command::new(bin)
            .args(args)
            .current_dir(cwd)
            .env("PRE_COMMIT_HOME", &self.pre_commit_home)
            .env("JJ_HOOKS_LOG", "info")
            .output()
            .unwrap()
    }

    /// Read all refs matching a glob from the primary git dir.
    pub fn refs_matching(&self, glob: &str) -> Vec<String> {
        let out = Command::new("git")
            .args(["for-each-ref", "--format=%(refname)", glob])
            .current_dir(&self.primary)
            .output()
            .unwrap();
        assert!(out.status.success(), "git for-each-ref failed");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|s| s.to_owned())
            .collect()
    }

    /// commit id pointed to by a ref in the primary git dir.
    pub fn rev_parse(&self, refname: &str) -> Option<String> {
        let out = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", refname])
            .current_dir(&self.primary)
            .output()
            .unwrap();
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// commit id on the remote for a given bookmark name (or None if absent).
    pub fn remote_commit(&self, bookmark: &str) -> Option<String> {
        let out = Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{bookmark}"),
            ])
            .current_dir(&self.remote)
            .output()
            .unwrap();
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// commit id of `<rev>` in the primary working copy's jj view.
    pub fn commit_id_of(&self, rev: &str) -> String {
        let out = capture_jj(
            &self.primary,
            &[
                "log",
                "--no-graph",
                "-r",
                rev,
                "-T",
                "commit_id",
                "--ignore-working-copy",
            ],
        );
        assert!(out.status.success(), "jj log failed: {}", show(&out));
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// Add a secondary workspace; returns its path.
    pub fn add_secondary(&self, name: &str) -> PathBuf {
        let path = self.tmp.path().join(name);
        run_jj(
            &self.primary,
            &["workspace", "add", path.to_str().unwrap(), "-r", "@-"],
        );
        path
    }
}

pub fn run(cwd: &Path, prog: &str, args: &[&str]) {
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

pub fn run_jj(cwd: &Path, args: &[&str]) {
    let out = capture_jj(cwd, args);
    if !out.status.success() {
        panic!("jj {args:?} failed in {cwd:?}:\n{}", show(&out));
    }
}

pub fn capture_jj(cwd: &Path, args: &[&str]) -> Output {
    Command::new("jj")
        .args(args)
        .args(["--color", "never"])
        .current_dir(cwd)
        .output()
        .unwrap()
}

pub fn show(out: &Output) -> String {
    format!(
        "exit={} stdout=\n{}stderr=\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    )
}

// -- hook yaml fixtures -------------------------------------------------------

pub const PRE_PUSH_PASSING: &str = r#"
repos:
  - repo: local
    hooks:
      - id: ok
        name: ok
        entry: 'true'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;

pub const PRE_PUSH_FAILING: &str = r#"
repos:
  - repo: local
    hooks:
      - id: fail
        name: fail
        entry: 'false'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;

/// A hook that writes a new file `AUTOFIX_RAN` into the worktree and exits 0.
/// Used to test the "hook modified files" path independently of exit status.
pub const PRE_PUSH_AUTOFIX: &str = r#"
repos:
  - repo: local
    hooks:
      - id: autofix
        name: autofix
        entry: sh -c 'echo fixed > AUTOFIX_RAN'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;
