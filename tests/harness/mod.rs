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
        run_jj(&primary, &["git", "push", "-b", "main"]);

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

    pub fn write_lefthook_config(&self, yaml: &str) {
        std::fs::write(self.primary.join("lefthook.yml"), yaml).unwrap();
    }

    pub fn write_hk_config(&self, pkl: &str) {
        std::fs::write(self.primary.join("hk.pkl"), pkl).unwrap();
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

    /// Same as `jj_hooks` but inject extra env vars before invocation.
    /// Used by tests that need a hook script to see custom env (e.g.
    /// the `PRE_PUSH_RECORD_RANGE` fixture's `JJ_HOOKS_TEST_RANGE_OUT`
    /// output-path indirection).
    pub fn jj_hooks_with_env(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let bin = env!("CARGO_BIN_EXE_jj-hooks");
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .current_dir(&self.primary)
            .env("PRE_COMMIT_HOME", &self.pre_commit_home)
            .env("JJ_HOOKS_LOG", "info");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    /// Run jj-hp under a sanitized PATH that includes only the binaries
    /// in `allow` (resolved against the parent process's PATH and
    /// symlinked into a tempdir). Used to test the "runner binary not
    /// on PATH" error path: write a hook config the autodetector
    /// recognises, then invoke with a PATH that's missing the runner
    /// binary and assert the error message is the new structured one
    /// instead of the raw `os error 2` from `posix_spawn`.
    ///
    /// `git`, `jj`, and `sh` should usually be in `allow` so jj-hp can
    /// still create the worktree and shell out for its own bookkeeping.
    pub fn jj_hooks_with_path_allowlist(&self, args: &[&str], allow: &[&str]) -> Output {
        self.jj_hooks_with_path_allowlist_and_extras(args, allow, &[])
    }

    /// Same as [`Self::jj_hooks_with_path_allowlist`] but also splice
    /// caller-provided executables into the sandbox bin dir.
    ///
    /// `extra_bins` is a slice of `(name, body)` pairs: each pair gets
    /// written as `sandbox-bin/<name>` with `body` as the script
    /// contents (shebang included; the harness chmod +x's it). Use
    /// this to plant fake runner binaries on the sandbox PATH that
    /// the resolver layers can find — e.g. a fake `prek` whose only
    /// job is to write a marker file so the test can assert which
    /// path the resolver picked.
    pub fn jj_hooks_with_path_allowlist_and_extras(
        &self,
        args: &[&str],
        allow: &[&str],
        extra_bins: &[(&str, &str)],
    ) -> Output {
        let bin_dir = self.tmp.path().join("sandbox-bin");
        // Recreating on every call keeps the test independent of order.
        let _ = std::fs::remove_dir_all(&bin_dir);
        std::fs::create_dir(&bin_dir).unwrap();
        let parent_path = std::env::var_os("PATH").expect("parent PATH must be set");
        for name in allow {
            let mut found = None;
            for dir in std::env::split_paths(&parent_path) {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    found = Some(candidate);
                    break;
                }
            }
            let src = found.unwrap_or_else(|| panic!("`{name}` not found on parent PATH"));
            #[cfg(unix)]
            std::os::unix::fs::symlink(&src, bin_dir.join(name)).unwrap();
            #[cfg(not(unix))]
            std::fs::copy(&src, bin_dir.join(name)).unwrap();
        }
        for (name, body) in extra_bins {
            let path = bin_dir.join(name);
            std::fs::write(&path, body).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&path).unwrap().permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&path, perms).unwrap();
            }
        }

        let bin = env!("CARGO_BIN_EXE_jj-hooks");
        Command::new(bin)
            .args(args)
            .current_dir(&self.primary)
            .env_clear()
            .env("PATH", &bin_dir)
            .env("PRE_COMMIT_HOME", &self.pre_commit_home)
            .env("JJ_HOOKS_LOG", "info")
            .env("HOME", self.tmp.path())
            .output()
            .unwrap()
    }

    /// Resolved sandbox-bin path. Tests that plant a fake binary via
    /// `jj_hooks_with_path_allowlist_and_extras` need this to point a
    /// shim or config value at the planted binary.
    pub fn sandbox_bin_dir(&self) -> std::path::PathBuf {
        self.tmp.path().join("sandbox-bin")
    }

    /// Look up `bin` on the parent process's PATH. Used by real-binary
    /// resolver tests to find the actual `prek` / `pre-commit` / `uv`
    /// executable to stage in the simulated venv path. Returns `None`
    /// when the binary isn't available — the caller should early-return
    /// (skip the test) in that case rather than fail, since the test
    /// is only meaningful when the binary is installed.
    pub fn find_on_parent_path(name: &str) -> Option<std::path::PathBuf> {
        let path = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    /// Copy (or symlink) a binary from the parent PATH into a tempdir
    /// outside any sandboxed PATH. Used to stage `prek` / `pre-commit`
    /// in a simulated `.venv/bin/` for the layer 1 / 2 resolver tests
    /// — the real binary stays fully invokable, but isn't reachable
    /// through the sandbox's PATH allowlist.
    ///
    /// We copy by default (some wrapper scripts behave oddly when
    /// invoked through symlinks); pass `symlink_only=true` to symlink
    /// instead (useful when the binary's argv0 self-resolution
    /// matters).
    pub fn stage_external_binary(
        &self,
        name: &str,
        venv_dir: &std::path::Path,
        symlink_only: bool,
    ) -> Option<std::path::PathBuf> {
        let src = Self::find_on_parent_path(name)?;
        std::fs::create_dir_all(venv_dir).unwrap();
        let dest = venv_dir.join(name);
        #[cfg(unix)]
        {
            if symlink_only {
                std::os::unix::fs::symlink(&src, &dest).unwrap();
            } else {
                // Hard-copy rather than symlink: some Nix-wrapper
                // binaries detect their argv0 path and refuse to run
                // through a symlink that isn't under the expected
                // store path. A plain copy of a shell-script wrapper
                // is fine; for ELF binaries it just wastes a few KB.
                std::fs::copy(&src, &dest).unwrap();
            }
        }
        #[cfg(not(unix))]
        std::fs::copy(&src, &dest).unwrap();
        Some(dest)
    }

    /// Write a fake runner binary inside the primary's `.git/hooks/<stage>`
    /// shim file using `prek install`'s exact format. The shim's `PREK=`
    /// line points at `bin_path` — the resolver's layer 2 should pick
    /// this up. Used to test the "prek lives in venv, no PATH" repro
    /// from issue #17 without actually creating a venv.
    pub fn write_prek_shim(&self, stage: &str, bin_path: &std::path::Path) {
        let git_dir = self.primary.join(".git");
        let hooks_dir = git_dir.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let shim_path = hooks_dir.join(stage);
        let body = format!(
            r#"#!/bin/sh
HERE="$(cd "$(dirname "$0")" && pwd)"
PREK="{}"
if [ ! -x "$PREK" ]; then
    PREK="prek"
fi
exec "$PREK" hook-impl --hook-dir "$HERE" --script-version 4 --hook-type={} -- "$@"
"#,
            bin_path.display(),
            stage,
        );
        std::fs::write(&shim_path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&shim_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&shim_path, perms).unwrap();
        }
    }

    /// Write a fake runner binary inside the primary's `.git/hooks/<stage>`
    /// shim file using `pre-commit install`'s exact format. The shim's
    /// `INSTALL_PYTHON=` line points at `python_path` (which the resolver
    /// then invokes as `python -mpre_commit`).
    pub fn write_pre_commit_shim(&self, stage: &str, python_path: &std::path::Path) {
        let git_dir = self.primary.join(".git");
        let hooks_dir = git_dir.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let shim_path = hooks_dir.join(stage);
        let body = format!(
            r#"#!/usr/bin/env bash
# start templated
INSTALL_PYTHON={}
ARGS=(hook-impl --config=.pre-commit-config.yaml --hook-type={})
# end templated
HERE="$(cd "$(dirname "$0")" && pwd)"
ARGS+=(--hook-dir "$HERE" -- "$@")
if [ -x "$INSTALL_PYTHON" ]; then
    exec "$INSTALL_PYTHON" -mpre_commit "${{ARGS[@]}}"
elif command -v pre-commit > /dev/null; then
    exec pre-commit "${{ARGS[@]}}"
else
    echo '`pre-commit` not found.' 1>&2
    exit 1
fi
"#,
            python_path.display(),
            stage,
        );
        std::fs::write(&shim_path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&shim_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&shim_path, perms).unwrap();
        }
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

    /// True iff jj has a commit with this commit_id in its view. After the
    /// auto-fixup pipeline we delete the temp git ref, but the commit
    /// stays in jj's commit graph and is still addressable by hash —
    /// this checks exactly that.
    pub fn jj_knows_commit(&self, commit_id: &str) -> bool {
        let out = capture_jj(
            &self.primary,
            &[
                "log",
                "--no-graph",
                "-r",
                commit_id,
                "-T",
                "commit_id",
                "--ignore-working-copy",
            ],
        );
        out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == commit_id
    }

    /// commit_id pointed to by the most recent fixup commit reachable
    /// from `bookmark` (the one that lives at `bookmark`'s parent after
    /// the fixup pipeline runs). Used by tests that need to find the
    /// fixup commit without going through a git ref.
    pub fn fixup_commit_for(&self, bookmark: &str) -> Option<String> {
        // The fixup commit's description is `jj-hooks: autofixes for <name>`
        // (per build_fixup_commit). Match by substring — globs trip on
        // the `:` and the space.
        let revset = format!("description(substring:'jj-hooks: autofixes for {bookmark}')");
        let out = capture_jj(
            &self.primary,
            &[
                "log",
                "--no-graph",
                "-r",
                &revset,
                "-T",
                "commit_id ++ \"\\n\"",
                "--ignore-working-copy",
                "--limit",
                "1",
            ],
        );
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if s.is_empty() { None } else { Some(s) }
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

    /// Add a fresh, empty bare git remote named `name` (at
    /// `<tmp>/<name>.git`) and register it as a git remote in the
    /// primary. Mirrors the origin setup in [`Self::new`] but leaves the
    /// remote with no refs at all — used to reproduce the first-push of a
    /// root-parented commit to a remote that shares no history (#284).
    pub fn add_empty_remote(&self, name: &str) -> PathBuf {
        let path = self.tmp.path().join(format!("{name}.git"));
        run(
            self.tmp.path(),
            "git",
            &["init", "--bare", "--quiet", path.to_str().unwrap()],
        );
        run(
            &self.primary,
            "git",
            &["remote", "add", name, path.to_str().unwrap()],
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

/// A hook that touches the index without changing file content. The
/// hook overwrites an existing tracked file with new content and
/// then restores the original content. `git add -A` then notes the
/// stat change in the index even though the file's final bytes
/// hash to the same blob; `git status --porcelain` reports the
/// file as modified; `git write-tree` produces a tree IDENTICAL to
/// the parent.
///
/// This simulates the runner-touched-the-index-but-didn't-change-
/// content false positive (issue #7), such as hk's stash + restore
/// lifecycle on a check-only run. Without the content-addressed
/// fixup gate, this would produce an empty fixup commit + abort
/// the push.
pub const PRE_PUSH_INDEX_TOUCH_ONLY: &str = r#"
repos:
  - repo: local
    hooks:
      - id: touch-only
        name: touch-only
        entry: sh -c 'cp existing.txt existing.txt.bak && echo "transient" > existing.txt && git add -A && cp existing.txt.bak existing.txt && rm existing.txt.bak'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;

/// A hook that fails AND creates a file the first time it runs, but
/// passes when the file already exists. Used to simulate the
/// retry-after-fixup recovery path: initial run produces a fixup
/// commit (the new file) and reports failure (the hook exited 1); a
/// re-run against the fixup commit sees the file present and exits 0.
///
/// This mirrors real-world racy hooks like hk's parallel flake-eval
/// steps + a separate auto-fixing markdownlint step in the same
/// invocation: the auto-fix is legitimate, the failure is transient,
/// and re-running against the fixup heals everything.
pub const PRE_PUSH_AUTOFIX_THEN_PASS: &str = r#"
repos:
  - repo: local
    hooks:
      - id: autofix-then-pass
        name: autofix-then-pass
        entry: sh -c 'if [ -e AUTOFIX_RAN ]; then exit 0; else echo fixed > AUTOFIX_RAN && exit 1; fi'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;
/// Used to assert what diff range jj-hooks computed for a given
/// revset — regression test for the bug where
/// `run_for_revset_outcome` only checked the tip slice of a
/// multi-commit revset instead of the full range.
///
/// The test sets `JJ_HOOKS_TEST_RANGE_OUT` to an absolute tempfile
/// path before invoking `jj-hp run`; the env var propagates
/// through `Command::new` → pre-commit → the hook shell.
///
/// Output file format (one var per line):
///
/// ```text
/// FROM=<sha>
/// TO=<sha>
/// ```
pub const PRE_PUSH_RECORD_RANGE: &str = r#"
repos:
  - repo: local
    hooks:
      - id: record-range
        name: record-range
        entry: sh -c 'printf "FROM=%s\nTO=%s\n" "$PRE_COMMIT_FROM_REF" "$PRE_COMMIT_TO_REF" > "$JJ_HOOKS_TEST_RANGE_OUT"'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;

/// A hook that records the `CARGO_TARGET_DIR` the child subprocess saw into
/// the file named by `$JJ_HOOKS_TEST_CTD_OUT`, then exits 0. Used by the
/// gate-cache (Mode A / T1) tests to assert the gate points cargo at the
/// PRIMARY workspace's `target/` (and to observe the byte-identical opt-out
/// fallback). Writes the literal `unset` when the var is absent so the
/// pre-change RED state (no injection) is observable.
pub const PRE_PUSH_RECORD_CARGO_TARGET_DIR: &str = r#"
repos:
  - repo: local
    hooks:
      - id: record-ctd
        name: record-ctd
        entry: sh -c 'printf "%s" "${CARGO_TARGET_DIR:-unset}" > "$JJ_HOOKS_TEST_CTD_OUT"'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;

/// A hook that writes the child's `CARGO_TARGET_DIR` to a per-bookmark file
/// `$JJ_HOOKS_WORKSPACE/ctd-<to-ref>` (keyed by `$PRE_COMMIT_TO_REF` so the N
/// concurrent batch children never collide on one path), then exits 0. Used
/// by the parallel batch gate-cache test — pre-commit suppresses hook STDOUT
/// on success, so a captured-output assertion is unreliable; a per-child file
/// under the shared primary root is not.
pub const PRE_PUSH_RECORD_CTD_PER_BOOKMARK: &str = r#"
repos:
  - repo: local
    hooks:
      - id: record-ctd-batch
        name: record-ctd-batch
        entry: sh -c 'printf "%s" "${CARGO_TARGET_DIR:-unset}" > "$JJ_HOOKS_WORKSPACE/ctd-$PRE_COMMIT_TO_REF"'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;

// -- lefthook fixtures --------------------------------------------------------
//
// Lefthook takes per-stage hooks under `<stage>:` and per-step commands
// under `commands:`. We give every command `glob: "*"` and `run: true|false|...`
// so they run unconditionally regardless of the file list jj-hooks
// computes.

pub const LEFTHOOK_PRE_PUSH_PASSING: &str = r#"pre-push:
  commands:
    ok:
      run: "true"
"#;

pub const LEFTHOOK_PRE_PUSH_FAILING: &str = r#"pre-push:
  commands:
    fail:
      run: "false"
"#;

pub const LEFTHOOK_PRE_PUSH_AUTOFIX: &str = r#"pre-push:
  commands:
    autofix:
      run: "echo fixed > AUTOFIX_RAN"
"#;

// -- hk fixtures --------------------------------------------------------------
//
// hk configs are written in pkl. To keep test fixtures hermetic we embed
// the upstream package URL the production hk.pkl uses, then define a
// single inline step per scenario.

const HK_PRELUDE: &str = r#"amends "package://github.com/jdx/hk/releases/download/v1.45.0/hk@1.45.0#/Config.pkl"
"#;

pub const HK_PRE_PUSH_PASSING: &str = r#"amends "package://github.com/jdx/hk/releases/download/v1.45.0/hk@1.45.0#/Config.pkl"

local linters = new Mapping<String, Step> {
    ["ok"] {
        glob = "*"
        check = "true"
    }
}

hooks {
    ["pre-push"] {
        fix = false
        steps = linters
    }
}
"#;

pub const HK_PRE_PUSH_FAILING: &str = r#"amends "package://github.com/jdx/hk/releases/download/v1.45.0/hk@1.45.0#/Config.pkl"

local linters = new Mapping<String, Step> {
    ["fail"] {
        glob = "*"
        check = "false"
    }
}

hooks {
    ["pre-push"] {
        fix = false
        steps = linters
    }
}
"#;

pub const HK_PRE_PUSH_AUTOFIX: &str = r#"amends "package://github.com/jdx/hk/releases/download/v1.45.0/hk@1.45.0#/Config.pkl"

local linters = new Mapping<String, Step> {
    ["autofix"] {
        glob = "*"
        check = "sh -c 'echo fixed > AUTOFIX_RAN'"
    }
}

hooks {
    ["pre-push"] {
        fix = false
        steps = linters
    }
}
"#;

// Silence the unused-const warning for HK_PRELUDE, which exists as
// documentation for what every HK_* fixture above starts with.
#[allow(dead_code)]
const _PRELUDE_ALIAS: &str = HK_PRELUDE;
