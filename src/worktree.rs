//! Ephemeral git worktree used to run hooks at a target commit without
//! disturbing the user's working copy or polluting the shared `.git/index`.
//!
//! Created via `git worktree add --detach`, removed on drop.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use tempfile::TempDir;
use tracing::warn;

use crate::error::{JjHooksError, Result};

/// Prefix used for each hook worktree directory.
pub const WORKTREE_PREFIX: &str = "jj-hooks-worktree-";
const CREATION_GRACE: Duration = Duration::from_secs(3600);
static SWEPT: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Default::default);

/// Resolve the on-disk root used for ephemeral hook worktrees.
pub fn worktree_root(jj: &crate::jj::JjCli) -> PathBuf {
    let env = std::env::var_os("JJ_HOOKS_WORKTREE_ROOT");
    let config = jj.run(&["config", "get", "jj-hooks.worktree-root"]).ok();
    let xdg_cache = std::env::var_os("XDG_CACHE_HOME");
    let home = std::env::home_dir();
    let tmp = std::env::temp_dir();

    for (source, path) in [
        ("JJ_HOOKS_WORKTREE_ROOT", env.as_deref().map(Path::new)),
        (
            "jj-hooks.worktree-root",
            config.as_deref().map(|value| Path::new(value.trim())),
        ),
    ] {
        if let Some(path) = path
            && !path.is_absolute()
        {
            warn!(
                "ignoring relative {source} worktree root: {}",
                path.display()
            );
        }
    }

    let root = root_from(
        env.as_deref(),
        config.as_deref(),
        xdg_cache.as_deref(),
        home.as_deref(),
        &tmp,
    );
    if root == tmp.join("jj-hooks-worktrees") {
        warn!(
            "jj-hp worktrees are using the temporary directory: {}",
            root.display()
        );
    }
    root
}

fn root_from(
    env: Option<&OsStr>,
    config: Option<&str>,
    xdg_cache: Option<&OsStr>,
    home: Option<&Path>,
    tmp: &Path,
) -> PathBuf {
    if let Some(path) = env.map(Path::new).filter(|path| path.is_absolute()) {
        return path.to_path_buf();
    }
    if let Some(path) = config
        .map(str::trim)
        .map(Path::new)
        .filter(|path| path.is_absolute())
    {
        return path.to_path_buf();
    }
    if let Some(path) = xdg_cache.map(Path::new).filter(|path| path.is_absolute()) {
        return path.join("jj-hooks").join("worktrees");
    }
    if let Some(home) = home {
        return home.join(".cache").join("jj-hooks").join("worktrees");
    }
    tmp.join("jj-hooks-worktrees")
}

/// Reap dead-owner hook worktrees under `root`; returns the number reaped.
pub(crate) fn sweep(root: &Path, grace: Duration) -> usize {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            warn!(
                "failed to read worktree root {} during sweep: {error}",
                root.display()
            );
            return 0;
        }
    };
    let mut checkouts = HashSet::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warn!(
                    "failed to read worktree root entry in {}: {error}",
                    root.display()
                );
                continue;
            }
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(WORKTREE_PREFIX) {
            continue;
        }
        if let Some(checkout_name) = name.strip_suffix(".lock") {
            checkouts.insert(root.join(checkout_name));
        } else if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            checkouts.insert(entry.path());
        }
    }

    let mut reaped = 0;
    for checkout in checkouts {
        let owner_path = lock_path(&checkout);
        let checkout_metadata = match fs::symlink_metadata(&checkout) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                warn!("skipping symlink worktree candidate {}", checkout.display());
                continue;
            }
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                warn!(
                    "failed to inspect worktree candidate {}: {error}",
                    checkout.display()
                );
                continue;
            }
        };

        match OpenOptions::new().read(true).write(true).open(&owner_path) {
            Ok(mut owner) => match owner.try_lock() {
                Err(std::fs::TryLockError::WouldBlock) => continue,
                Err(std::fs::TryLockError::Error(error)) => {
                    warn!(
                        "failed to lock worktree owner file {}: {error}",
                        owner_path.display()
                    );
                    continue;
                }
                Ok(()) => {
                    let mut contents = Vec::new();
                    if let Err(error) = owner
                        .seek(SeekFrom::Start(0))
                        .and_then(|_| owner.read_to_end(&mut contents))
                    {
                        warn!(
                            "failed to read worktree owner file {}: {error}",
                            owner_path.display()
                        );
                        continue;
                    }
                    if contents.is_empty() {
                        if is_younger_than(&owner_path, &checkout, grace) {
                            continue;
                        }
                        let checkout_removed = match checkout_metadata {
                            None => true,
                            Some(metadata) if !metadata.is_dir() => false,
                            Some(_) => match fs::remove_dir(&checkout) {
                                Ok(()) => true,
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                                Err(error) => {
                                    warn!(
                                        "failed to remove empty worktree directory {}: {error}",
                                        checkout.display()
                                    );
                                    false
                                }
                            },
                        };
                        if checkout_removed {
                            match fs::remove_file(&owner_path) {
                                Ok(()) => reaped += 1,
                                Err(error) => warn!(
                                    "failed to remove empty worktree owner file {}: {error}",
                                    owner_path.display()
                                ),
                            }
                        }
                        continue;
                    }

                    let Some(git_dir) = parse_owner(&contents) else {
                        warn!(
                            "malformed worktree owner file {}; skipping",
                            owner_path.display()
                        );
                        continue;
                    };
                    if let Some(metadata) = &checkout_metadata
                        && !metadata.is_dir()
                    {
                        warn!(
                            "skipping non-directory worktree candidate {}",
                            checkout.display()
                        );
                        continue;
                    }
                    let registered = match registered_worktree(&git_dir, &checkout) {
                        Ok(registered) => registered,
                        Err(error) => {
                            warn!(
                                "failed to verify worktree registration for {}: {error}",
                                checkout.display()
                            );
                            continue;
                        }
                    };
                    if registered {
                        let output = match Command::new("git")
                            .arg(format!("--git-dir={}", git_dir.display()))
                            .args(["worktree", "remove", "--force", "--force"])
                            .arg(&checkout)
                            .output()
                        {
                            Ok(output) => output,
                            Err(error) => {
                                warn!(
                                    "failed to remove dead-owner git worktree {}: {error}",
                                    checkout.display()
                                );
                                continue;
                            }
                        };
                        if !output.status.success() {
                            warn!(
                                "git worktree remove failed for {}: {}",
                                checkout.display(),
                                String::from_utf8_lossy(&output.stderr)
                            );
                            continue;
                        }
                        if checkout_metadata.is_some() {
                            match fs::remove_dir_all(&checkout) {
                                Ok(()) => {}
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                                Err(error) => {
                                    warn!(
                                        "failed to remove dead-owner worktree directory {}: {error}",
                                        checkout.display()
                                    );
                                    continue;
                                }
                            }
                        }
                    } else if let Some(metadata) = checkout_metadata {
                        if !metadata.is_dir() {
                            warn!(
                                "unregistered worktree candidate {} is not a directory; skipping",
                                checkout.display()
                            );
                            continue;
                        }
                        let is_empty = match fs::read_dir(&checkout) {
                            Ok(mut entries) => match entries.next() {
                                None => true,
                                Some(Ok(_)) => false,
                                Some(Err(error)) => {
                                    warn!(
                                        "failed to inspect unregistered worktree directory {}: {error}",
                                        checkout.display()
                                    );
                                    continue;
                                }
                            },
                            Err(error) => {
                                warn!(
                                    "failed to inspect unregistered worktree directory {}: {error}",
                                    checkout.display()
                                );
                                continue;
                            }
                        };
                        if !is_empty {
                            warn!(
                                "unregistered worktree candidate {} is not empty; skipping",
                                checkout.display()
                            );
                            continue;
                        }
                        if let Err(error) = fs::remove_dir(&checkout) {
                            warn!(
                                "failed to remove empty unregistered worktree directory {}: {error}",
                                checkout.display()
                            );
                            continue;
                        }
                    }
                    if unlink_owner(&owner_path) {
                        reaped += 1;
                    }
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if is_younger_than(&checkout, &checkout, grace) {
                    continue;
                }
                match fs::remove_dir(&checkout) {
                    Ok(()) => reaped += 1,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => warn!(
                        "failed to remove lockless worktree directory {}: {error}",
                        checkout.display()
                    ),
                }
            }
            Err(error) => warn!(
                "failed to open worktree owner file {}: {error}",
                owner_path.display()
            ),
        }
    }
    reaped
}

fn registered_worktree(git_dir: &Path, checkout: &Path) -> std::io::Result<bool> {
    let output = Command::new("git")
        .arg(format!("--git-dir={}", git_dir.display()))
        .args(["worktree", "list", "--porcelain"])
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "git worktree list failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let checkout = canonicalize_missing_path(checkout)?;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(path) = line.strip_prefix("worktree ") else {
            continue;
        };
        let listed = match canonicalize_missing_path(Path::new(path)) {
            Ok(listed) => listed,
            Err(_) => continue,
        };
        if listed == checkout {
            return Ok(true);
        }
    }
    Ok(false)
}

fn canonicalize_missing_path(path: &Path) -> std::io::Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            let name = path.file_name().ok_or(error)?;
            Ok(fs::canonicalize(parent)?.join(name))
        }
        Err(error) => Err(error),
    }
}

fn unlink_owner(owner_path: &Path) -> bool {
    match fs::remove_file(owner_path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            warn!(
                "failed to remove worktree owner file {}: {error}",
                owner_path.display()
            );
            false
        }
    }
}

fn is_younger_than(path: &Path, fallback: &Path, grace: Duration) -> bool {
    fs::metadata(path)
        .or_else(|_| fs::metadata(fallback))
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| modified.elapsed().unwrap_or_default() < grace)
}

fn parse_owner(contents: &[u8]) -> Option<PathBuf> {
    let separator = contents.iter().position(|byte| *byte == b'\n')?;
    let (pid, rest) = contents.split_at(separator);
    if std::str::from_utf8(pid).ok()?.parse::<u32>().ok()? == 0 {
        return None;
    }
    let git_dir = rest.get(1..)?.strip_suffix(b"\n")?;
    if git_dir.is_empty() || git_dir.contains(&b'\n') {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(OsString::from_vec(git_dir.to_vec())))
    }
    #[cfg(not(unix))]
    {
        Some(PathBuf::from(std::str::from_utf8(git_dir).ok()?))
    }
}

fn lock_path(checkout: &Path) -> PathBuf {
    let mut path: OsString = checkout.as_os_str().to_owned();
    path.push(".lock");
    PathBuf::from(path)
}

fn write_owner(file: &mut File, git_dir: &Path) -> std::io::Result<()> {
    writeln!(file, "{}", std::process::id())?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        file.write_all(git_dir.as_os_str().as_bytes())?;
    }
    #[cfg(not(unix))]
    file.write_all(git_dir.to_string_lossy().as_bytes())?;
    file.write_all(b"\n")
}

/// Process-wide lock for `git worktree add` invocations.
///
/// The lock prevents concurrent git worktree metadata creation races.
static WORKTREE_CREATE_LOCK: Mutex<()> = Mutex::new(());

pub struct Worktree {
    git_dir: PathBuf,
    checkout: TempDir,
    lock_path: PathBuf,
    owner: File,
    removed: bool,
}

impl Worktree {
    /// Create a detached worktree at `commit` using the given primary git dir.
    pub fn create(root: &Path, git_dir: &Path, commit: &str) -> Result<Self> {
        let root_existed = root.exists();
        fs::create_dir_all(root)?;
        if !root_existed {
            let parent = root
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
                warn!(
                    "failed to sync worktree root parent {}: {error}",
                    parent.display()
                );
            }
        }
        {
            let mut swept = SWEPT
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if swept.insert(root.to_path_buf()) {
                sweep(root, CREATION_GRACE);
            }
        }
        let checkout = TempDir::with_prefix_in(WORKTREE_PREFIX, root)?;
        let lock_path = lock_path(checkout.path());
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lock_path)?;
        if let Err(error) = owner.lock() {
            drop(owner);
            let _ = fs::remove_file(&lock_path);
            return Err(error.into());
        }
        let mut worktree = Self {
            git_dir: git_dir.to_owned(),
            checkout,
            lock_path,
            owner,
            removed: false,
        };
        let mut git_add_failed = false;
        let result = (|| {
            write_owner(&mut worktree.owner, git_dir)?;
            worktree.owner.sync_all()?;
            if let Err(error) = File::open(root).and_then(|root_dir| root_dir.sync_all()) {
                warn!("failed to sync worktree root {}: {error}", root.display());
            }

            // Serialize git worktree metadata operations across concurrent workers.
            let _guard = WORKTREE_CREATE_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let output = Command::new("git")
                .arg(format!("--git-dir={}", git_dir.display()))
                .args(["worktree", "add", "--detach", "--quiet"])
                .arg(worktree.checkout.path())
                .arg(commit)
                .output()?;
            drop(_guard);

            if !output.status.success() {
                git_add_failed = true;
                return Err(JjHooksError::JjFailed {
                    status: output.status.code().unwrap_or(-1),
                    stderr: format!(
                        "git worktree add failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    ),
                });
            }
            Ok(())
        })();

        if let Err(error) = result {
            let registered = if git_add_failed {
                match registered_worktree(git_dir, worktree.checkout.path()) {
                    Ok(registered) => registered,
                    Err(check_error) => {
                        warn!(
                            "failed to verify worktree registration after git add failed: {check_error}"
                        );
                        true
                    }
                }
            } else {
                false
            };
            if registered {
                if let Err(cleanup_error) = worktree.remove() {
                    warn!("failed to clean up worktree after creation failed: {cleanup_error}");
                }
            } else if let Err(cleanup_error) = worktree.remove_locally() {
                warn!(
                    "failed to clean up unregistered worktree after creation failed: {cleanup_error}"
                );
            }
            return Err(error);
        }
        Ok(worktree)
    }

    pub fn path(&self) -> &Path {
        self.checkout.path()
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    fn remove_locally(&mut self) -> std::io::Result<()> {
        if self.checkout.path().exists() {
            fs::remove_dir_all(self.checkout.path())?;
        }
        match fs::remove_file(&self.lock_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.removed = true;
        Ok(())
    }

    fn remove(&mut self) -> std::io::Result<()> {
        if self.removed {
            return Ok(());
        }
        let _guard = WORKTREE_CREATE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let output = Command::new("git")
            .arg(format!("--git-dir={}", self.git_dir.display()))
            .args(["worktree", "remove", "--force", "--force"])
            .arg(self.checkout.path())
            .output()?;
        drop(_guard);

        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "git worktree remove failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        if self.checkout.path().exists() {
            fs::remove_dir_all(self.checkout.path())?;
        }
        fs::remove_file(&self.lock_path)?;
        self.removed = true;
        Ok(())
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        if let Err(e) = self.remove() {
            warn!(
                "failed to clean up worktree at {}: {e}",
                self.checkout.path().display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Worktree, lock_path, root_from, sweep};
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;
    use tempfile::TempDir;

    fn setup_repo() -> (TempDir, PathBuf, PathBuf) {
        let temp = TempDir::new().unwrap();
        let primary = temp.path().join("primary");
        std::fs::create_dir(&primary).unwrap();
        run(&primary, "git", &["init", "--quiet"]);
        std::fs::write(primary.join("file"), "content\n").unwrap();
        run(&primary, "git", &["add", "file"]);
        run(
            &primary,
            "git",
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "initial",
            ],
        );
        let git_dir = primary.join(".git");
        (temp, primary, git_dir)
    }

    fn run(cwd: &Path, program: &str, args: &[&str]) {
        let output = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program} {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn create_worktree(root: &Path, git_dir: &Path) -> Worktree {
        Worktree::create(root, git_dir, "HEAD").unwrap()
    }

    fn add_registered_worktree(root: &Path, git_dir: &Path, name: &str) -> PathBuf {
        let path = root.join(format!("{}{name}", super::WORKTREE_PREFIX));
        let output = Command::new("git")
            .arg(format!("--git-dir={}", git_dir.display()))
            .args(["worktree", "add", "--detach", "--quiet"])
            .arg(&path)
            .arg("HEAD")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let owner_path = lock_path(&path);
        std::fs::write(
            &owner_path,
            format!("{}\n{}\n", std::process::id(), git_dir.display()),
        )
        .unwrap();
        path
    }

    fn listed_worktree_count(primary: &Path) -> usize {
        let listing = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(primary)
            .output()
            .unwrap();
        assert!(listing.status.success());
        String::from_utf8_lossy(&listing.stdout)
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count()
    }

    #[test]
    fn stale_prunable_worktree_does_not_block_dead_owner_sweep() {
        let (_temp, primary, git_dir) = setup_repo();
        let root = _temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let stale = _temp.path().join("a-stale-parent").join("stale-worktree");
        let added = Command::new("git")
            .arg(format!("--git-dir={}", git_dir.display()))
            .args(["worktree", "add", "--detach", "--quiet"])
            .arg(&stale)
            .arg("HEAD")
            .output()
            .unwrap();
        assert!(
            added.status.success(),
            "{}",
            String::from_utf8_lossy(&added.stderr)
        );
        std::fs::remove_dir_all(stale.parent().unwrap()).unwrap();
        assert!(!stale.exists());
        assert!(!stale.parent().unwrap().exists());
        let dead = add_registered_worktree(&root, &git_dir, "after-stale");
        let listing = Command::new("git")
            .arg(format!("--git-dir={}", git_dir.display()))
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap();
        assert!(listing.status.success());
        let listing = String::from_utf8_lossy(&listing.stdout);
        assert!(listing.contains(&format!("worktree {}", stale.display())));
        assert!(super::registered_worktree(&git_dir, &dead).unwrap());
        let lock = lock_path(&dead);
        assert_eq!(listed_worktree_count(&primary), 3);
        assert_eq!(sweep(&root, Duration::ZERO), 1);
        assert!(!dead.exists());
        assert!(!lock.exists());
        assert_eq!(listed_worktree_count(&primary), 2);
    }

    #[test]
    fn failed_post_checkout_add_removes_registered_worktree() {
        use std::os::unix::fs::PermissionsExt;

        let (temp, primary, git_dir) = setup_repo();
        let marker = temp.path().join("post-checkout-ran");
        let hooks = temp.path().join("hooks");
        std::fs::create_dir(&hooks).unwrap();
        let post_checkout = hooks.join("post-checkout");
        std::fs::write(
            &post_checkout,
            format!("#!/bin/sh\nprintf ran > '{}'\nexit 1\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&post_checkout, std::fs::Permissions::from_mode(0o755)).unwrap();
        run(
            &primary,
            "git",
            &["config", "core.hooksPath", hooks.to_str().unwrap()],
        );

        let root = temp.path().join("root");
        let result = Worktree::create(&root, &git_dir, "HEAD");
        assert!(
            result.is_err(),
            "post-checkout hook should make git worktree add fail"
        );
        assert!(marker.exists(), "post-checkout hook did not run");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        assert_eq!(listed_worktree_count(&primary), 1);
    }

    #[test]
    fn failed_add_leaves_no_checkout_or_lock() {
        let (temp, _primary, git_dir) = setup_repo();
        let root = temp.path().join("root");
        assert!(Worktree::create(&root, &git_dir, "no-such-commit").is_err());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }

    #[test]
    fn sweep_skips_held_owner_lock() {
        let temp = TempDir::new().unwrap();
        let owner_path;
        {
            let (_repo_temp, _primary, git_dir) = setup_repo();
            let worktree = create_worktree(temp.path(), &git_dir);
            owner_path = lock_path(worktree.path());
            assert_eq!(sweep(temp.path(), Duration::ZERO), 0);
            assert!(worktree.path().exists());
        }
        assert!(!owner_path.exists());
    }

    #[test]
    fn sweep_reaps_unlocked_worktree_with_admin_entry() {
        let (_temp, primary, git_dir) = setup_repo();
        let root = _temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let path = add_registered_worktree(&root, &git_dir, "sweep");
        let owner_path = lock_path(&path);
        assert_eq!(listed_worktree_count(&primary), 2);
        assert_eq!(sweep(&root, Duration::ZERO), 1);
        assert!(!path.exists());
        assert!(!owner_path.exists());
        assert_eq!(listed_worktree_count(&primary), 1);
    }

    #[test]
    fn sweep_reaps_worktree_through_symlinked_root() {
        let (_temp, primary, git_dir) = setup_repo();
        let root = _temp.path().join("root");
        let alias = _temp.path().join("alias");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let path = add_registered_worktree(&root, &git_dir, "symlink");
        let owner_path = lock_path(&path);
        assert_eq!(listed_worktree_count(&primary), 2);
        assert_eq!(sweep(&alias, Duration::ZERO), 1);
        assert!(!path.exists());
        assert!(!owner_path.exists());
        assert_eq!(listed_worktree_count(&primary), 1);
    }

    #[test]
    fn sweep_orphan_owner_lock_removes_git_admin_entry() {
        let (_temp, primary, git_dir) = setup_repo();
        let root = _temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        // Dead owner: registered with git, checkout gone, lock not held.
        let path = root.join(format!("{}orphan", super::WORKTREE_PREFIX));
        let added = Command::new("git")
            .arg(format!("--git-dir={}", git_dir.display()))
            .args(["worktree", "add", "--detach"])
            .arg(&path)
            .arg("HEAD")
            .output()
            .unwrap();
        assert!(added.status.success(), "{added:?}");
        assert_eq!(listed_worktree_count(&primary), 2);
        std::fs::remove_dir_all(&path).unwrap();
        let lock_path = lock_path(&path);
        std::fs::write(
            &lock_path,
            format!("{}\n{}\n", std::process::id(), git_dir.display()),
        )
        .unwrap();
        assert_eq!(sweep(&root, Duration::ZERO), 1);
        assert!(!lock_path.exists());
        assert_eq!(listed_worktree_count(&primary), 1);
    }

    #[test]
    fn sweep_preserves_foreign_nonempty_directory_with_forged_owner() {
        let (_temp, _primary, git_dir) = setup_repo();
        let root = _temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let checkout = root.join(format!("{}foreign", super::WORKTREE_PREFIX));
        std::fs::create_dir(&checkout).unwrap();
        std::fs::write(checkout.join("keep"), "foreign data").unwrap();
        let owner_path = lock_path(&checkout);
        std::fs::write(
            &owner_path,
            format!("{}\n{}\n", std::process::id(), git_dir.display()),
        )
        .unwrap();

        assert_eq!(sweep(&root, Duration::ZERO), 0);
        assert_eq!(
            std::fs::read_to_string(checkout.join("keep")).unwrap(),
            "foreign data"
        );
        assert!(owner_path.exists());
    }

    #[test]
    fn sweep_preserves_lockless_registered_user_worktree() {
        let (_temp, primary, _git_dir) = setup_repo();
        let path = _temp.path().join("root").join("jj-hooks-worktree-user");
        std::fs::create_dir_all(&path).unwrap();
        run(
            &primary,
            "git",
            &[
                "worktree",
                "add",
                "--detach",
                "--quiet",
                path.to_str().unwrap(),
                "HEAD",
            ],
        );
        assert_eq!(sweep(path.parent().unwrap(), Duration::ZERO), 0);
        assert!(path.exists());
        let listing = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(primary)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&listing.stdout)
                .lines()
                .filter(|line| line.starts_with("worktree "))
                .count(),
            2
        );
    }

    #[test]
    fn sweep_removes_old_empty_checkout_and_lock_but_obeys_grace() {
        let temp = TempDir::new().unwrap();
        let recent = temp.path().join("jj-hooks-worktree-recent");
        std::fs::create_dir(&recent).unwrap();
        std::fs::write(lock_path(&recent), "").unwrap();
        assert_eq!(sweep(temp.path(), Duration::from_secs(3600)), 0);
        assert!(recent.exists());
        let old = temp.path().join("jj-hooks-worktree-old");
        std::fs::create_dir(&old).unwrap();
        let old_lock = lock_path(&old);
        std::fs::write(&old_lock, "").unwrap();
        assert_eq!(sweep(temp.path(), Duration::ZERO), 2);
        assert!(!old.exists());
        assert!(!old_lock.exists());
    }

    #[test]
    fn root_from_prefers_absolute_env_override() {
        let root = root_from(
            Some(OsStr::new("/env")),
            Some("/config"),
            Some(OsStr::new("/xdg")),
            Some(Path::new("/home/user")),
            Path::new("/tmp"),
        );
        assert_eq!(root, PathBuf::from("/env"));
    }

    #[test]
    fn root_from_ignores_relative_env_and_uses_config() {
        let root = root_from(
            Some(OsStr::new("relative")),
            Some(" /config "),
            Some(OsStr::new("/xdg")),
            Some(Path::new("/home/user")),
            Path::new("/tmp"),
        );
        assert_eq!(root, PathBuf::from("/config"));
    }

    #[test]
    fn root_from_ignores_relative_config_and_uses_absolute_xdg() {
        let root = root_from(
            None,
            Some("relative"),
            Some(OsStr::new("/xdg")),
            Some(Path::new("/home/user")),
            Path::new("/tmp"),
        );
        assert_eq!(root, PathBuf::from("/xdg/jj-hooks/worktrees"));
    }

    #[test]
    fn root_from_falls_back_from_relative_xdg_to_home() {
        let root = root_from(
            None,
            None,
            Some(OsStr::new("relative")),
            Some(Path::new("/home/user")),
            Path::new("/tmp"),
        );
        assert_eq!(root, PathBuf::from("/home/user/.cache/jj-hooks/worktrees"));
    }

    #[test]
    fn root_from_falls_back_to_temp_without_home() {
        let root = root_from(None, None, None, None, Path::new("/tmp"));
        assert_eq!(root, PathBuf::from("/tmp/jj-hooks-worktrees"));
    }
}
