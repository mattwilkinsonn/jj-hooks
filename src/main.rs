use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use jj_hooks::cli::{Cli, Command, RunnerArg};
use jj_hooks::error::JjHooksError;
use jj_hooks::init::{self, InteractivePrompter};
use jj_hooks::jj::JjCli;
use jj_hooks::push::{execute_push, maybe_advance_bookmarks, run_checks};
use jj_hooks::runner::{Runner, Stage};

fn main() -> ExitCode {
    let cli = Cli::parse();

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cli.log_level)),
        )
        .with_target(false)
        .without_time()
        .try_init();

    match real_main(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("jj-hooks: {e}");
            ExitCode::from(1)
        }
    }
}

fn real_main(cli: Cli) -> Result<ExitCode, JjHooksError> {
    let jj = JjCli::new(std::env::current_dir()?);

    match cli.command {
        Command::Push {
            dry_run,
            advance_bookmarks,
            stage,
            push_args,
        } => {
            let workspace_root = jj.workspace_root()?;
            let Some(runner) = resolve_runner(cli.runner, &workspace_root)? else {
                tracing::info!("no hook-runner config detected; falling through to jj git push");
                execute_push(&jj, &push_args, dry_run)?;
                return Ok(ExitCode::SUCCESS);
            };

            let report = run_checks(&jj, &workspace_root, runner, stage.into(), &push_args)?;

            if report.skipped {
                execute_push(&jj, &push_args, dry_run)?;
                return Ok(ExitCode::SUCCESS);
            }

            for (update, outcome) in &report.per_bookmark {
                if !outcome.success {
                    eprintln!("jj-hooks: {update}: hook failed");
                }
                if let Some(commit) = &outcome.fixup_commit {
                    eprintln!("jj-hooks: {update}: hooks modified files (fixup commit {commit})");
                }
            }

            let advance = advance_bookmarks || advance_bookmarks_from_config(&jj);
            let advanced = maybe_advance_bookmarks(&jj, &report, advance)?;
            for name in advanced {
                eprintln!("jj-hooks: advanced bookmark {name} to fixup commit");
            }

            if report.any_failure() || report.any_fixup() {
                eprintln!("jj-hooks: aborting push");
                return Ok(ExitCode::from(1));
            }

            execute_push(&jj, &push_args, dry_run)?;
            Ok(ExitCode::SUCCESS)
        }

        Command::Run { stage, revset } => {
            let workspace_root = jj.workspace_root()?;
            let Some(runner) = resolve_runner(cli.runner, &workspace_root)? else {
                tracing::info!("no hook-runner config detected; nothing to do");
                return Ok(ExitCode::SUCCESS);
            };

            run_against_revset(&jj, &workspace_root, runner, stage.into(), &revset)
        }

        Command::Init => {
            let detected = jj
                .workspace_root()
                .ok()
                .and_then(|root| Runner::autodetect(&root).ok().flatten());
            let mut prompter = InteractivePrompter;
            let plan = init::plan(detected, &mut prompter)?;
            let outcome = init::apply(&plan, None, None)?;
            if outcome.alias_set {
                eprintln!("jj-hooks: installed `aliases.push` = jj-hooks push");
            }
            if outcome.advance_bookmarks_set {
                eprintln!("jj-hooks: set `jj-hooks.advance-bookmarks = true`");
            }
            let jjui = outcome.jjui_actions_added;
            if jjui.added_jj_push
                || jjui.added_jj_push_selected
                || jjui.added_binding_x_p
                || jjui.added_binding_x_p_caps
            {
                eprintln!("jj-hooks: merged jjui actions/bindings into jjui config");
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn resolve_runner(
    flag: Option<RunnerArg>,
    workspace_root: &std::path::Path,
) -> Result<Option<Runner>, JjHooksError> {
    if let Some(r) = flag {
        // User asked for a specific runner — honor it exactly.
        return Ok(Some(r.into()));
    }
    let autodetected = Runner::autodetect(workspace_root)?;
    Ok(autodetected.map(|r| {
        // prek is a faster drop-in for pre-commit; prefer it when present.
        jj_hooks::runner::prefer_prek_when_available(r, jj_hooks::runner::prek_on_path())
    }))
}

fn advance_bookmarks_from_config(jj: &JjCli) -> bool {
    matches!(
        jj.run(&["config", "get", "jj-hooks.advance-bookmarks"])
            .ok()
            .map(|s| s.trim().to_owned()),
        Some(ref v) if v == "true"
    )
}

fn run_against_revset(
    jj: &JjCli,
    workspace_root: &std::path::Path,
    runner: Runner,
    stage: Stage,
    revset: &str,
) -> Result<ExitCode, JjHooksError> {
    use jj_hooks::hooks;
    use jj_hooks::jj as jj_mod;

    // Resolve revset to (parent_commit, target_commit) bounds. We pick the
    // first commit in the revset as the target and its first parent as
    // the diff base.
    let target = jj.run(&[
        "log",
        "--no-graph",
        "-r",
        revset,
        "-T",
        "commit_id",
        "--limit",
        "1",
        "--ignore-working-copy",
    ])?;
    let target = target.trim();
    if target.is_empty() {
        eprintln!("jj-hooks: revset `{revset}` is empty");
        return Ok(ExitCode::from(2));
    }

    let parent = jj.run(&[
        "log",
        "--no-graph",
        "-r",
        &format!("{target}-"),
        "-T",
        "commit_id",
        "--limit",
        "1",
        "--ignore-working-copy",
    ])?;
    let parent = parent.trim().to_owned();

    let update = jj_hooks::bookmark_updates::BookmarkUpdate {
        remote: "<local>".into(),
        bookmark: format!("revset:{revset}"),
        update_type: jj_hooks::bookmark_updates::UpdateType::MoveForward,
        old_commit: Some(parent),
        new_commit: Some(target.to_owned()),
    };

    let primary_git_dir = jj_mod::primary_git_dir(workspace_root)?;
    let outcome = hooks::run_for_update(jj, &primary_git_dir, runner, stage, &update)?;

    if let Some(commit) = &outcome.fixup_commit {
        eprintln!("jj-hooks: hooks modified files (fixup commit {commit})");
    }
    if outcome.success && outcome.fixup_commit.is_none() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}
