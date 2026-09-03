//! Library entrypoint shared by the `jj-hooks` and `jj-hp` binaries.
//!
//! Both binaries are identical — `jj-hp` is just a shorter name that's
//! easier to type and that we route the `jj push` alias through.

pub mod bookmark_updates;
pub mod cli;
pub mod completions;
pub mod error;
pub mod gate_cache;
pub mod hooks;
pub mod init;
pub mod jj;
pub mod push;
pub mod push_tags;
pub mod repo_env;
pub mod runner;
pub mod setup;
pub mod worktree;

use std::process::ExitCode;

use clap::FromArgMatches;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Command};
use crate::error::JjHooksError;
use crate::init::InteractivePrompter;
use crate::jj::JjCli;
use crate::push::{execute_push, maybe_advance_bookmarks, run_checks};
use crate::runner::{Runner, Stage};

/// Parse CLI args, dispatch to a subcommand, and return the process exit
/// code. Both `bin/jj-hooks` and `bin/jj-hp` are trivial wrappers around
/// this function.
pub fn run() -> ExitCode {
    // Handle dynamic completion requests *before* anything else. When the
    // shell calls us back with `COMPLETE=<shell>` set (via the script
    // emitted by the `completions` subcommand), CompleteEnv runs the
    // ArgValueCompleter callbacks and exits — we never reach `Cli::parse`.
    use clap::CommandFactory;
    clap_complete::CompleteEnv::with_factory(Cli::command).complete();

    // Dispatch CLI parsing through a command whose `name` matches the
    // invoked binary name (argv[0]'s file_name). Both `jj-hooks` and
    // `jj-hp` share this entrypoint, so without this swap clap's
    // `#[command(name = "jj-hooks")]` would make `jj-hp --version` print
    // `jj-hooks 0.3.x` — wrong identifier, and the homebrew tap formula
    // test catches it. Bonus: `--help` headers are also self-correct.
    let bin_name = std::env::args()
        .next()
        .and_then(|arg0| {
            std::path::Path::new(&arg0)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "jj-hooks".into());
    // clap's `Command::name`/`bin_name` require `Into<Str>` which only
    // accepts `&'static str` (not `&str` with a shorter lifetime). The
    // `bin_name` String is built from argv[0] at runtime; leak it once
    // so the slice satisfies the lifetime bound. The leak is process-
    // lifetime (one allocation per `run()` call, which is at most one
    // per process), so it's effectively free.
    let bin_name_static: &'static str = Box::leak(bin_name.into_boxed_str());
    let cmd = Cli::command()
        .name(bin_name_static)
        .bin_name(bin_name_static);
    let cli = Cli::from_arg_matches(&cmd.get_matches()).unwrap_or_else(|e| e.exit());

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cli.log_level)),
        )
        .with_target(false)
        .without_time()
        .try_init();

    match dispatch(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("jj-hooks: {e}");
            ExitCode::from(1)
        }
    }
}

fn dispatch(cli: Cli) -> Result<ExitCode, JjHooksError> {
    let jj = JjCli::new(std::env::current_dir()?);

    match cli.command {
        Command::Push {
            advance_bookmarks,
            stage,
            push,
            dry_run,
            no_retry_after_fixup,
        } => {
            let workspace_root = jj.workspace_root()?;
            // Argv that's just the bookmark selection (no --dry-run) — used
            // for the dry-run probe that figures out which bookmarks would
            // change. Adding --dry-run here would double up since the probe
            // already adds it.
            let select_argv = crate::cli::push_argv(&push, false);
            // Argv used to actually push (includes --dry-run if requested).
            let push_argv = crate::cli::push_argv(&push, dry_run);

            // Resolve the runner per-update inside `run_checks` so a
            // runner-migration commit (e.g. one that deletes lefthook.yml
            // and adds hk.pkl) is gated by the runner the *target* commit
            // commits to, not the runner the primary workspace happens
            // to have on disk right now. The `--runner` CLI flag still
            // overrides this for users who need to force a specific runner.
            let cli_runner: Option<Runner> = cli.runner.map(Into::into);

            let run_opts = crate::hooks::RunOpts {
                retry_after_fixup: !no_retry_after_fixup,
                // push always uses the diff range — the bookmark's ref
                // bounds are the whole point.
                all_files: false,
                // jj-hp push is a single-bookmark CLI invocation; the
                // user wants the runner's live progress bar in their
                // terminal. Capture is only needed by the multi-update
                // parallel batch API used by jj-gt.
                capture_output: false,
            };

            let report = run_checks(
                &jj,
                &workspace_root,
                cli_runner,
                stage.into(),
                &select_argv,
                run_opts,
            )?;

            if report.skipped {
                execute_push(&jj, &push_argv, false)?;
                return Ok(ExitCode::SUCCESS);
            }

            for (update, outcome) in &report.per_bookmark {
                if !outcome.success {
                    eprintln!("jj-hooks: {update}: hook failed");
                    // Setup-step failures synthesize a `captured_output`
                    // buffer (the captured stdout/stderr plus a
                    // trailing line naming the failing step) so the
                    // user has the context they need to fix it.
                    // Regular hook failures in live (non-capture) mode
                    // have already streamed their output to the
                    // terminal, so they have `captured_output: None`
                    // here. Either way: dump the buffer when present.
                    if let Some(captured) = &outcome.captured_output {
                        for line in captured.lines() {
                            eprintln!("│ {line}");
                        }
                    }
                }
                if let Some(commit) = &outcome.fixup_commit {
                    if outcome.success && outcome.retried {
                        // Final state is good — the retry on the fixup
                        // was clean — but the initial run failed, so
                        // warn the user about the racy step.
                        eprintln!(
                            "jj-hooks: {update}: hooks modified files; re-run on fixup commit \
                             was clean (fixup {commit})"
                        );
                    } else {
                        eprintln!(
                            "jj-hooks: {update}: hooks modified files (fixup commit {commit})"
                        );
                    }
                } else if outcome.success && outcome.initial_failure {
                    // Edge case: initial run failed without producing a
                    // fixup, retry-after-fixup never triggered. Surface
                    // the initial failure for context.
                    eprintln!("jj-hooks: {update}: initial hook run reported a failure");
                }
            }

            let advance = advance_bookmarks || advance_bookmarks_from_config(&jj);
            let advanced = maybe_advance_bookmarks(&jj, &report, advance)?;
            for name in advanced {
                eprintln!("jj-hooks: advanced bookmark {name} to fixup commit");
            }

            // Abort when any bookmark either fails outright or has a
            // fixup commit the user hasn't squashed in yet. A successful
            // retry-after-fixup still produces a fixup_commit (the user
            // needs to advance the bookmark to it before re-pushing), so
            // it correctly aborts here.
            if report.any_failure() || report.any_fixup() {
                eprintln!("jj-hooks: aborting push");
                return Ok(ExitCode::from(1));
            }

            execute_push(&jj, &push_argv, false)?;
            Ok(ExitCode::SUCCESS)
        }

        Command::Run {
            stage,
            revset,
            no_retry_after_fixup,
            all_files,
        } => {
            let workspace_root = jj.workspace_root()?;
            // Same per-worktree autodetect contract as the push path: the
            // runner is picked from the target commit's own tree, not from
            // the primary workspace. `--runner` overrides.
            let cli_runner: Option<Runner> = cli.runner.map(Into::into);

            let run_opts = crate::hooks::RunOpts {
                retry_after_fixup: !no_retry_after_fixup,
                all_files,
                capture_output: false,
            };

            run_for_revset(
                &jj,
                &workspace_root,
                cli_runner,
                stage.into(),
                &revset,
                run_opts,
            )
        }

        Command::PushTags {
            tags,
            all,
            force,
            dry_run,
            remote,
        } => {
            push_tags::run(
                &jj,
                push_tags::PushTagsOpts {
                    remote: &remote,
                    tags,
                    all,
                    force,
                    dry_run,
                },
            )?;
            Ok(ExitCode::SUCCESS)
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
                eprintln!("jj-hooks: installed `aliases.push` = jj-hp push");
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

        Command::Completions { shell } => {
            use clap::CommandFactory;
            use clap_complete::env::EnvCompleter;
            use clap_complete::env::{Bash, Elvish, Fish, Powershell, Zsh};

            let cmd = Cli::command();
            // Pick the binary name dynamically from argv[0] so the script
            // targets whichever name the user invoked (`jj-hooks` vs `jj-hp`).
            let bin_name = std::env::args()
                .next()
                .and_then(|arg0| {
                    std::path::Path::new(&arg0)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "jj-hp".into());

            // Write the env-driven registration script (NOT the static
            // completion script). Static scripts can't fire ArgValueCompleter
            // callbacks, so bookmark / remote completion would silently fall
            // through to file completion. The env-driven script makes the
            // shell call us back with `COMPLETE=<shell>` set, which the
            // CompleteEnv::complete() call at the top of run() handles.
            let mut out = std::io::stdout();
            let result =
                match shell {
                    clap_complete::Shell::Bash => Bash
                        .write_registration("COMPLETE", &bin_name, &bin_name, &bin_name, &mut out),
                    clap_complete::Shell::Zsh => Zsh
                        .write_registration("COMPLETE", &bin_name, &bin_name, &bin_name, &mut out),
                    clap_complete::Shell::Fish => Fish
                        .write_registration("COMPLETE", &bin_name, &bin_name, &bin_name, &mut out),
                    clap_complete::Shell::PowerShell => Powershell
                        .write_registration("COMPLETE", &bin_name, &bin_name, &bin_name, &mut out),
                    clap_complete::Shell::Elvish => Elvish
                        .write_registration("COMPLETE", &bin_name, &bin_name, &bin_name, &mut out),
                    _ => {
                        eprintln!("jj-hooks: unsupported shell for dynamic completion");
                        return Ok(ExitCode::from(2));
                    }
                };
            // Use cmd to satisfy the unused warning. The script writers
            // above don't need it — they reference the binary by name only.
            let _ = cmd;
            result.map_err(JjHooksError::Io)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn advance_bookmarks_from_config(jj: &JjCli) -> bool {
    matches!(
        jj.run(&["config", "get", "jj-hooks.advance-bookmarks"])
            .ok()
            .map(|s| s.trim().to_owned()),
        Some(ref v) if v == "true"
    )
}

/// Run the configured hook runner against a jj revset, the same way
/// `jj-hp run [REVSET]` does. Exposed as a library entrypoint so other
/// tools (e.g. `jj-gt`) can gate their own pipelines on the same hook
/// machinery without shelling out to the `jj-hp` binary.
///
/// Resolves the latest commit in `revset` as the "to" target and uses
/// its parent as the "from" diff base. The hook backend is picked from
/// the target commit's tree (so a runner-migration commit is gated by
/// the runner the *target* commits to), unless `cli_runner` overrides.
///
/// Returns `ExitCode::SUCCESS` only when every hook step exits 0 *and*
/// no fixup commit was produced (i.e. hooks didn't modify any files).
/// Otherwise returns a non-zero exit code suitable for propagating from
/// a binary's `main`.
pub fn run_for_revset(
    jj: &JjCli,
    workspace_root: &std::path::Path,
    cli_runner: Option<Runner>,
    stage: Stage,
    revset: &str,
    opts: hooks::RunOpts,
) -> Result<ExitCode, JjHooksError> {
    match run_for_revset_outcome(jj, workspace_root, cli_runner, stage, revset, opts)? {
        None => {
            eprintln!("jj-hooks: revset `{revset}` is empty");
            Ok(ExitCode::from(2))
        }
        Some(outcome) => {
            if let Some(commit) = &outcome.fixup_commit {
                if outcome.success && outcome.retried {
                    eprintln!(
                        "jj-hooks: hooks modified files; re-run on fixup commit was clean \
                         (fixup {commit})"
                    );
                } else {
                    eprintln!("jj-hooks: hooks modified files (fixup commit {commit})");
                }
            } else if outcome.success && outcome.initial_failure {
                eprintln!("jj-hooks: initial hook run reported a failure");
            }
            if outcome.success && outcome.fixup_commit.is_none() {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(1))
            }
        }
    }
}

/// Structured variant of [`run_for_revset`] — returns `Ok(None)` for
/// an empty revset, otherwise the per-update [`hooks::HookOutcome`].
///
/// Callers (other binaries that compose jj-hooks into their own
/// pipelines) typically want to branch on `outcome.success` and
/// `outcome.fixup_commit` rather than parse an exit code.
///
/// The synthesized [`bookmark_updates::BookmarkUpdate`] uses the
/// *full revset* as the diff range:
///
/// - `new_commit` (the "to" / target tree the hooks see) is the
///   single head of the revset (`heads(<revset>)`). A multi-head
///   revset is rejected upstream — the worktree we materialise to
///   run hooks against can only be one commit.
/// - `old_commit` (the "from" / diff base the hooks compare
///   against) is the parent of the lowest commit in the revset
///   (`roots(<revset>)-`). For `main..tip` this is `main` itself,
///   so hooks see the entire stack diff `main..tip` — same as what
///   `git push origin tip` would push.
///
/// For single-commit revsets like `@` or `<sha>` this reduces to
/// `parent → target`, the same shape the old per-tip implementation
/// produced.
pub fn run_for_revset_outcome(
    jj: &JjCli,
    workspace_root: &std::path::Path,
    cli_runner: Option<Runner>,
    stage: Stage,
    revset: &str,
    opts: hooks::RunOpts,
) -> Result<Option<hooks::HookOutcome>, JjHooksError> {
    // Head of the revset = the tip commit. `heads(...)` returns the
    // unique commit in the set that no other commit in the set is
    // an ancestor of; for a linear chain this is the topmost
    // commit. For a multi-head revset jj will return multiple
    // results; we limit to 1 and let the caller surface a
    // confusing-but-not-wrong outcome rather than failing here
    // (multi-head pre-push checks aren't a workflow this library
    // tries to support).
    let target = jj.run(&[
        "log",
        "--no-graph",
        "-r",
        &format!("heads({revset})"),
        "-T",
        "commit_id",
        "--limit",
        "1",
        "--ignore-working-copy",
    ])?;
    let target = target.trim();
    if target.is_empty() {
        return Ok(None);
    }

    // From-ref = parent of the lowest commit in the revset. For
    // `main..tip` this resolves to `main` itself, so hooks see the
    // entire stack range. For single-commit revsets like `@`,
    // `roots(@)-` reduces to `@-` — same shape the old code
    // produced.
    let parent = jj.run(&[
        "log",
        "--no-graph",
        "-r",
        &format!("roots({revset})-"),
        "-T",
        "commit_id",
        "--limit",
        "1",
        "--ignore-working-copy",
    ])?;
    let parent = parent.trim().to_owned();

    let update = bookmark_updates::BookmarkUpdate {
        remote: "<local>".into(),
        bookmark: format!("revset:{revset}"),
        update_type: bookmark_updates::UpdateType::MoveForward,
        old_commit: Some(parent),
        new_commit: Some(target.to_owned()),
    };

    let primary_git_dir = jj::primary_git_dir(workspace_root)?;
    let outcome = hooks::run_for_update(
        jj,
        &primary_git_dir,
        workspace_root,
        cli_runner,
        stage,
        &update,
        opts,
    )?;
    Ok(Some(outcome))
}
