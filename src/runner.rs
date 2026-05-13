//! Hook runner backends.
//!
//! Each runner has slightly different CLI ergonomics, so this module owns
//! the per-backend knowledge of "what args do I accept". pre-commit and
//! prek share a CLI shape; hk has its own; lefthook needs a file list
//! rather than ref bounds.

use std::path::{Path, PathBuf};

use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    PreCommit,
    Prek,
    Lefthook,
    Hk,
}

impl Runner {
    pub fn bin(self) -> &'static str {
        match self {
            Runner::PreCommit => "pre-commit",
            Runner::Prek => "prek",
            Runner::Lefthook => "lefthook",
            Runner::Hk => "hk",
        }
    }

    /// Filesystem probe for runner config files at `root`. Returns Ok(Some)
    /// for a single match, Ok(None) for no match, Err for ambiguous.
    pub fn autodetect(root: &Path) -> Result<Option<Runner>> {
        let candidates = [
            (Runner::Hk, &["hk.pkl"][..]),
            (
                Runner::Lefthook,
                &[
                    "lefthook.yml",
                    "lefthook.yaml",
                    ".lefthook.yml",
                    ".lefthook.yaml",
                ][..],
            ),
            (
                Runner::PreCommit,
                &[".pre-commit-config.yaml", ".pre-commit-config.yml"][..],
            ),
        ];

        let mut found: Vec<Runner> = Vec::new();
        for (runner, files) in candidates {
            if files.iter().any(|f| root.join(f).exists()) {
                found.push(runner);
            }
        }

        match found.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(*one)),
            many => Err(crate::error::JjHooksError::Parse(format!(
                "multiple hook-runner configs found at workspace root: {:?}. Use --runner to pick one.",
                many.iter().map(|r| r.bin()).collect::<Vec<_>>()
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    PreCommit,
    PrePush,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::PreCommit => "pre-commit",
            Stage::PrePush => "pre-push",
        }
    }
}

/// Build the argv for a hook invocation against the from..to ref range.
///
/// pre-commit / prek: `<bin> run --hook-stage <stage> --from-ref <from> --to-ref <to>`.
/// hk: `hk run <stage> --from-ref <from> --to-ref <to>` — hk takes the
/// same `--from-ref` / `--to-ref` flags as pre-commit, and *needs* them
/// when running in an ephemeral worktree (otherwise hk tries to resolve
/// `refs/remotes/origin/HEAD` and errors out).
///
/// Lefthook needs a file list, not refs — use [`lefthook_command`] instead.
pub fn hook_command(runner: Runner, stage: Stage, from: &str, to: &str) -> Vec<String> {
    match runner {
        Runner::PreCommit | Runner::Prek => vec![
            runner.bin().into(),
            "run".into(),
            "--hook-stage".into(),
            stage.as_str().into(),
            "--from-ref".into(),
            from.into(),
            "--to-ref".into(),
            to.into(),
        ],
        Runner::Hk => vec![
            runner.bin().into(),
            "run".into(),
            stage.as_str().into(),
            "--from-ref".into(),
            from.into(),
            "--to-ref".into(),
            to.into(),
        ],
        Runner::Lefthook => panic!(
            "lefthook does not take ref bounds; use lefthook_command with a file list instead"
        ),
    }
}

/// Build the argv for a lefthook invocation. Lefthook accepts repeated
/// `--file <path>` flags (one per changed file). When the file list is
/// empty we omit the flags entirely and let lefthook decide whether
/// "nothing to do" is a success or no-op.
pub fn lefthook_command(stage: Stage, files: &[PathBuf]) -> Vec<String> {
    let mut argv = vec!["lefthook".into(), "run".into(), stage.as_str().into()];
    for f in files {
        argv.push("--file".into());
        argv.push(f.to_string_lossy().into_owned());
    }
    argv
}

/// Swap `Runner::PreCommit` for `Runner::Prek` when prek is on the user's
/// PATH. prek is a drop-in pre-commit replacement that's much faster, so
/// users who happen to have both installed should get the faster one
/// automatically. An explicit `--runner pre-commit` short-circuits this
/// (callers should only invoke `prefer_prek_when_available` on the
/// autodetected result, not on a user-supplied override).
pub fn prefer_prek_when_available(autodetected: Runner, prek_present: bool) -> Runner {
    match (autodetected, prek_present) {
        (Runner::PreCommit, true) => Runner::Prek,
        _ => autodetected,
    }
}

/// Probe `$PATH` for the `prek` binary.
pub fn prek_on_path() -> bool {
    which("prek").is_some()
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
