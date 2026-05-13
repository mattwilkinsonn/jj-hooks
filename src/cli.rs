//! clap argument structs for the `jj-hooks` binary.

use clap::{Parser, Subcommand};
use clap_complete::Shell;

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
    /// Run hooks then push. Mirrors the flags `jj git push` accepts; any
    /// flags we don't model can be passed through after `--`.
    Push {
        /// Advance the local bookmark to the fixup commit when hooks modify
        /// files. Reads `jj-hooks.advance-bookmarks` config when not given.
        #[arg(long)]
        advance_bookmarks: bool,

        /// Hook stage to run. Defaults to `pre-push`.
        #[arg(long, value_enum, default_value = "pre-push")]
        stage: StageArg,

        #[command(flatten)]
        push: PushArgs,

        /// Only display what will change on the remote.
        #[arg(long)]
        dry_run: bool,
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

    /// Print a shell completion script. Pipe into your shell rc, e.g.
    /// `eval "$(jj-hp completions zsh)"`.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: Shell,
    },
}

/// All flags that `jj git push` accepts and that we model explicitly.
/// Anything unrecognized passes through via [`PushArgs::passthrough`] (the
/// trailing `--` escape hatch).
#[derive(clap::Args, Debug, Clone, Default)]
pub struct PushArgs {
    /// Push only this bookmark (can be repeated).
    #[arg(
        short = 'b',
        long,
        action = clap::ArgAction::Append,
        add = clap_complete::ArgValueCompleter::new(crate::completions::bookmark_value_completer),
    )]
    pub bookmark: Vec<String>,

    /// Push bookmarks pointing to these commits (can be repeated).
    #[arg(short = 'r', long, action = clap::ArgAction::Append)]
    pub revision: Vec<String>,

    /// Push these commits by creating a bookmark (can be repeated).
    #[arg(short = 'c', long, action = clap::ArgAction::Append)]
    pub change: Vec<String>,

    /// The remote to push to.
    #[arg(
        long,
        add = clap_complete::ArgValueCompleter::new(crate::completions::remote_value_completer),
    )]
    pub remote: Option<String>,

    /// Push all bookmarks (including new bookmarks).
    #[arg(long)]
    pub all: bool,

    /// Push all tracked bookmarks.
    #[arg(long)]
    pub tracked: bool,

    /// Push all deleted bookmarks.
    #[arg(long)]
    pub deleted: bool,

    /// Allow pushing new bookmarks (i.e. bookmarks not yet on the remote).
    #[arg(long)]
    pub allow_new: bool,

    /// Pass-through args after `--` forwarded to `jj git push` verbatim.
    #[arg(last = true)]
    pub passthrough: Vec<String>,
}

/// Reconstruct the argv list for `jj git push` from our parsed flags.
/// Known flags emit their canonical form; unknown flags arrive via
/// `passthrough` and are appended at the end.
pub fn push_argv(args: &PushArgs, dry_run: bool) -> Vec<String> {
    let mut out = Vec::new();

    for b in &args.bookmark {
        out.push("-b".into());
        out.push(b.clone());
    }
    for r in &args.revision {
        out.push("-r".into());
        out.push(r.clone());
    }
    for c in &args.change {
        out.push("-c".into());
        out.push(c.clone());
    }
    if let Some(remote) = &args.remote {
        out.push("--remote".into());
        out.push(remote.clone());
    }
    if args.all {
        out.push("--all".into());
    }
    if args.tracked {
        out.push("--tracked".into());
    }
    if args.deleted {
        out.push("--deleted".into());
    }
    if args.allow_new {
        out.push("--allow-new".into());
    }
    if dry_run {
        out.push("--dry-run".into());
    }
    out.extend(args.passthrough.iter().cloned());

    out
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
