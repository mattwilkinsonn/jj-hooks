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
            (Runner::Hk, &["hk.pkl"][..]),
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
/// pre-commit / prek share a CLI: `<bin> run --hook-stage <stage> --from-ref <from> --to-ref <to>`.
/// hk uses positional stage and no ref args: `hk run <stage>`.
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
        Runner::Hk => vec![runner.bin().into(), "run".into(), stage.as_str().into()],
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
