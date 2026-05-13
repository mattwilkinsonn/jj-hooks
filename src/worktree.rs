//! Ephemeral git worktree used to run hooks at a target commit without
//! disturbing the user's working copy or polluting the shared `.git/index`.
//!
//! Created via `git worktree add --detach`, removed on drop.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use tracing::warn;

use crate::error::{JjHooksError, Result};

pub struct Worktree {
    git_dir: PathBuf,
    dir: TempDir,
    removed: bool,
}

impl Worktree {
    /// Create a detached worktree at `commit` using the given primary git dir.
    pub fn create(git_dir: &Path, commit: &str) -> Result<Self> {
        let dir = TempDir::with_prefix("jj-hooks-worktree-")?;

        let output = Command::new("git")
            .arg(format!("--git-dir={}", git_dir.display()))
            .args(["worktree", "add", "--detach", "--quiet"])
            .arg(dir.path())
            .arg(commit)
            .output()?;

        if !output.status.success() {
            return Err(JjHooksError::JjFailed {
                status: output.status.code().unwrap_or(-1),
                stderr: format!(
                    "git worktree add failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }

        Ok(Self {
            git_dir: git_dir.to_owned(),
            dir,
            removed: false,
        })
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    fn remove(&mut self) -> std::io::Result<()> {
        if self.removed {
            return Ok(());
        }
        let output = Command::new("git")
            .arg(format!("--git-dir={}", self.git_dir.display()))
            .args(["worktree", "remove", "--force"])
            .arg(self.dir.path())
            .output()?;

        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "git worktree remove failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        self.removed = true;
        Ok(())
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if let Err(e) = self.remove() {
            warn!(
                "failed to clean up worktree at {}: {e}",
                self.dir.path().display()
            );
        }
    }
}
