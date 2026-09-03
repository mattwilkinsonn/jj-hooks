//! Ephemeral git worktree used to run hooks at a target commit without
//! disturbing the user's working copy or polluting the shared `.git/index`.
//!
//! Created via `git worktree add --detach`, removed on drop.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use tempfile::TempDir;
use tracing::warn;

use crate::error::{JjHooksError, Result};

/// Process-wide lock for `git worktree add` invocations.
///
/// macOS / APFS deterministically races concurrent `git worktree add
/// --detach` calls against the same primary git dir — one of them
/// reads a partially-initialized `.git/worktrees/<name>/commondir`
/// and fails with "Undefined error: 0". Linux serializes the
/// equivalent via filesystem-level index lock and tends to hide the
/// race, but it's still a race.
///
/// The lock only covers the `git worktree add` call itself —
/// hook *execution* inside the worktree still runs in parallel
/// (the actual expensive work). Worktree creation is fast (<100ms)
/// so serializing it doesn't measurably reduce the parallel
/// speedup, and the alternative ("trust git's locking") is
/// already known to fail on macOS in CI.
static WORKTREE_CREATE_LOCK: Mutex<()> = Mutex::new(());

pub struct Worktree {
    git_dir: PathBuf,
    dir: TempDir,
    removed: bool,
}

impl Worktree {
    /// Create a detached worktree at `commit` using the given primary git dir.
    pub fn create(git_dir: &Path, commit: &str) -> Result<Self> {
        let dir = TempDir::with_prefix("jj-hooks-worktree-")?;

        // Serialize concurrent `git worktree add` calls — see
        // [`WORKTREE_CREATE_LOCK`] for the rationale. The lock is
        // released as soon as `git worktree add` returns; we don't
        // hold it for the lifetime of the Worktree.
        let _guard = WORKTREE_CREATE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let output = Command::new("git")
            .arg(format!("--git-dir={}", git_dir.display()))
            .args(["worktree", "add", "--detach", "--quiet"])
            .arg(dir.path())
            .arg(commit)
            .output()?;
        drop(_guard);

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
        // Same locking rationale as `create` — git's worktree
        // metadata directory is shared across worktrees and
        // concurrent updates race on macOS.
        let _guard = WORKTREE_CREATE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let output = Command::new("git")
            .arg(format!("--git-dir={}", self.git_dir.display()))
            .args(["worktree", "remove", "--force"])
            .arg(self.dir.path())
            .output()?;
        drop(_guard);

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
