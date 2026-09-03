//! Subprocess wrapper around the `jj` CLI plus utilities for locating the
//! primary git directory that a (primary or secondary) workspace is colocated
//! with.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{JjHooksError, Result};

/// Wrapper around the `jj` CLI rooted at a particular workspace directory.
#[derive(Debug, Clone)]
pub struct JjCli {
    cwd: PathBuf,
}

impl JjCli {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self { cwd: cwd.into() }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Run a `jj` subcommand and capture stdout. Stderr is captured only on
    /// failure (and surfaced via the error type); on success it is inherited
    /// so progress / hints reach the user's terminal.
    pub fn run(&self, args: &[&str]) -> Result<String> {
        self.run_inner(args, /*capture_stderr_always=*/ false)
    }

    /// Same as `run`, but always captures stderr so the caller can parse it
    /// (e.g. `jj git push --dry-run` writes its output to stderr).
    pub fn run_capture_stderr(&self, args: &[&str]) -> Result<String> {
        self.run_inner(args, /*capture_stderr_always=*/ true)
    }

    fn run_inner(&self, args: &[&str], capture_stderr_always: bool) -> Result<String> {
        let output = Command::new("jj")
            .args(args)
            .args(["--color", "never"])
            .current_dir(&self.cwd)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(JjHooksError::JjFailed {
                status: output.status.code().unwrap_or(-1),
                stderr,
            });
        }

        if capture_stderr_always {
            // Caller wants stderr (e.g. dry-run output). Concatenate so they
            // can see everything jj produced.
            let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
            Ok(combined)
        } else {
            // Surface jj's hints to the user's terminal even on success.
            if !output.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&output.stderr));
            }
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
    }

    pub fn workspace_root(&self) -> Result<PathBuf> {
        let out = self.run(&["workspace", "root", "--ignore-working-copy"])?;
        // `dunce::canonicalize` resolves symlinks like `std::fs::canonicalize`
        // but strips the Windows `\\?\` verbatim prefix when the path fits the
        // classic form. Git-for-Windows rejects `\\?\`-prefixed `--git-dir`
        // values with "not a git repository", so every path we hand to git
        // must go through dunce, not std. See issue #94.
        Ok(dunce::canonicalize(PathBuf::from(out.trim()))?)
    }
}

/// Resolve the path to the primary git directory associated with the
/// workspace rooted at `workspace_root` (which may be either the primary
/// workspace or a secondary one).
///
/// jj's layout:
/// - Primary workspace: `.jj/repo/` is a directory containing `store/git_target`.
/// - Secondary workspace: `.jj/repo` is a *file* containing a relative path
///   pointing back at the primary's `.jj/repo` directory.
///
/// `store/git_target` itself is a relative path written relative to the
/// *store* directory.
pub fn primary_git_dir(workspace_root: &Path) -> Result<PathBuf> {
    let repo = resolve_repo_dir(workspace_root)?;
    let store = repo.join("store");
    let target_file = store.join("git_target");
    let relative = std::fs::read_to_string(&target_file)?;
    let relative = relative.trim();

    let resolved = dunce::canonicalize(store.join(relative)).map_err(|e| {
        JjHooksError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "could not resolve primary git dir from {} (contents: {relative:?}): {e}",
                target_file.display()
            ),
        ))
    })?;

    Ok(resolved)
}

/// Resolve `<workspace>/.jj/repo` to the actual repo directory, following
/// the secondary-workspace pointer file if present.
fn resolve_repo_dir(workspace_root: &Path) -> Result<PathBuf> {
    let repo = workspace_root.join(".jj/repo");
    let meta = std::fs::metadata(&repo)?;
    if meta.is_dir() {
        return Ok(dunce::canonicalize(&repo)?);
    }
    let pointer = std::fs::read_to_string(&repo)?;
    let pointer = pointer.trim();
    Ok(dunce::canonicalize(
        repo.parent()
            .expect(".jj/repo always has a parent")
            .join(pointer),
    )?)
}
