//! clap argument structs for the `jj-hooks` binary.

use clap::{Parser, Subcommand};

use crate::runner::{Runner, Stage};

#[derive(Parser, Debug)]
#[command(
    name = "jj-hooks",
    about = "Run pre-commit / lefthook / hk hooks against jj bookmark pushes",
    version,
    propagate_version = true
)]
pub struct Cli {
    /// Hook runner to use. Overrides autodetect.
    #[arg(long, value_enum, global = true, env = "JJ_HOOKS_RUNNER")]
    pub runner: Option<RunnerArg>,

    /// Log level filter (e.g. `info`, `debug`, `warn`).
    #[arg(long, global = true, env = "JJ_HOOKS_LOG", default_value = "warn")]
    pub log_level: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run hooks then push. Mirrors `jj git push` flags after `--`.
    Push {
        /// Pass through `--dry-run` to `jj git push`.
        #[arg(long)]
        dry_run: bool,

        /// Advance the local bookmark to the fixup commit when hooks modify
        /// files. Reads `jj-hooks.advance-bookmarks` config when not given.
        #[arg(long)]
        advance_bookmarks: bool,

        /// Hook stage to run. Defaults to `pre-push`.
        #[arg(long, value_enum, default_value = "pre-push")]
        stage: StageArg,

        /// Arguments forwarded to `jj git push`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        push_args: Vec<String>,
    },

    /// Run hooks against a revset without pushing.
    Run {
        /// Hook stage to run. Defaults to `pre-commit`.
        #[arg(long, value_enum, default_value = "pre-commit")]
        stage: StageArg,

        /// Revset to check. Defaults to `@`.
        #[arg(default_value = "@")]
        revset: String,
    },

    /// Interactive setup: install `jj push` alias and configure defaults.
    Init,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
pub enum RunnerArg {
    PreCommit,
    Prek,
    Lefthook,
    Hk,
}

impl From<RunnerArg> for Runner {
    fn from(value: RunnerArg) -> Self {
        match value {
            RunnerArg::PreCommit => Runner::PreCommit,
            RunnerArg::Prek => Runner::Prek,
            RunnerArg::Lefthook => Runner::Lefthook,
            RunnerArg::Hk => Runner::Hk,
        }
    }
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
pub enum StageArg {
    PreCommit,
    PrePush,
}

impl From<StageArg> for Stage {
    fn from(value: StageArg) -> Self {
        match value {
            StageArg::PreCommit => Stage::PreCommit,
            StageArg::PrePush => Stage::PrePush,
        }
    }
}
