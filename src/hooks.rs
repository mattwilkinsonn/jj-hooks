//! Per-bookmark hook execution pipeline.
//!
//! For each bookmark update being pushed:
//! 1. Resolve one or more `from_ref` commits (the ancestors on the remote).
//! 2. Create an ephemeral detached worktree at the new commit.
//! 3. Run the configured hook backend against each `from_ref` in turn.
//!    Modifications accumulate in the same worktree.
//! 4. If the worktree ended up with modifications, build a fixup commit
//!    via `git commit-tree`, anchor it under `refs/jj-hooks/fixup/<bookmark>`,
//!    and `jj git import` so jj sees it.
//! 5. Optionally re-run the hook backend against the fixup commit; if
//!    the re-run is clean, the overall outcome is reported as success
//!    with `initial_failure = true` so callers can surface the
//!    transient failure. See [`RunOpts::retry_after_fixup`].
//! 6. Optionally advance the bookmark to the fixup commit.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::bookmark_updates::BookmarkUpdate;
use crate::error::{JjHooksError, Result};
use crate::jj::JjCli;
use crate::runner::{
    Runner, Stage, hook_command, hook_command_all_files, lefthook_command,
    lefthook_command_all_files,
};
use crate::setup::{self, SetupStep};
use crate::worktree::Worktree;

/// Cooperative cancellation handle for parallel hook runs.
///
/// `run_once` checks this between each hook-runner subprocess
/// invocation (per-from-ref iteration in the diff-range path, the
/// single call in the all-files path) and between the runner and the
/// fixup-commit step. If cancellation has been requested, the
/// outcome short-circuits with `success: true` and no captured
/// output — the caller knows the run was cancelled because it
/// requested it, and the no-op return keeps the result-collection
/// loop in `run_for_partitioned_updates_parallel` simple.
///
/// `Cancel::never()` produces a no-op token for callers that never
/// want to cancel (the per-bookmark `jj-hp push` CLI path). The
/// `Default` impl gives the same.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// A fresh cancellation token in the un-cancelled state.
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// A token that never fires. Cheaper than `new()` only in
    /// readability — both allocate one `AtomicBool`.
    pub fn never() -> Self {
        Self::new()
    }

    /// Mark this token as cancelled. Idempotent.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Has cancellation been requested?
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
pub struct HookOutcome {
    /// Final success for this bookmark — `true` iff every hook run we
    /// took into account exited 0. When `retry_after_fixup` is enabled
    /// and a retry on the fixup commit was clean, this reports `true`
    /// even though the initial run failed.
    pub success: bool,
    /// Commit id of the fixup commit if the hook(s) modified files.
    /// `Some(_)` means the caller's tree is stale relative to what the
    /// hooks want.
    pub fixup_commit: Option<String>,
    /// `true` iff we re-ran hooks against the fixup commit after the
    /// initial run reported failure-with-fixup.
    pub retried: bool,
    /// `true` iff the initial hook run exited non-zero, regardless of
    /// whether a subsequent retry healed the outcome. CLI uses this to
    /// warn the user that something was racy even when the final state
    /// is OK.
    pub initial_failure: bool,
    /// Captured stdout/stderr from every hook subprocess invoked for
    /// this update, in order. `None` when [`RunOpts::capture_output`]
    /// is false (the default — hook output streams straight to the
    /// parent's terminal so the user sees runner progress live).
    /// `Some(buf)` when the caller asked for capture so it can
    /// multiplex N parallel runs into ordered output blocks. See
    /// [`run_for_updates_parallel`] for the canonical consumer.
    pub captured_output: Option<String>,
    /// `true` iff the pipeline observed cancellation between
    /// subprocess invocations and short-circuited the remaining
    /// runs. The partitioned-parallel entrypoint flips its
    /// partition's `Cancel` when any sibling fails; the user sees a
    /// "cancelled" annotation in the output rather than treating
    /// this as a normal success/failure.
    pub cancelled: bool,
}

/// Inputs that control how [`run_for_update`] behaves. Defaults match
/// pre-0.3.0 behavior (no retry).
#[derive(Debug, Clone, Copy, Default)]
pub struct RunOpts {
    /// When the initial hook run produces a fixup commit AND reports
    /// failure, re-run the hooks against the fixup commit. If the
    /// re-run is clean, the overall outcome is reported as success
    /// with `initial_failure = true`. Use this to recover from
    /// transient races (e.g. hk's intra-bookmark step parallelism
    /// fighting for `.git/index.lock` while one step legitimately
    /// auto-fixes files).
    pub retry_after_fixup: bool,
    /// Run hooks against every tracked file in the worktree rather
    /// than the diff range. Each runner gets its own all-files flag
    /// (see [`crate::runner::hook_command_all_files`]). Currently
    /// surfaced via `jj-hp run --all-files`; `push` always uses the
    /// diff range since the bookmark's ref bounds are the whole
    /// point.
    pub all_files: bool,
    /// Capture hook subprocess stdout/stderr into the returned
    /// [`HookOutcome::captured_output`] instead of letting it stream
    /// straight to the parent's terminal. Required for parallel
    /// per-bookmark hook runs (see [`run_for_updates_parallel`]) so
    /// N concurrent runs don't garble the terminal; the caller
    /// replays the captured blocks in completion order.
    ///
    /// Default is `false` — sequential single-bookmark runs (the
    /// `jj-hp push` path) want the live runner progress bar.
    pub capture_output: bool,
}

/// Run hooks for one bookmark update. Returns the outcome (success +
/// optional fixup commit + retry metadata).
///
/// `cli_runner` is the user's `--runner` override (or `None` for autodetect).
/// When `None`, runner detection happens inside the ephemeral worktree at the
/// target commit — so a commit that migrated runners (e.g. `lefthook → hk`)
/// is gated by the runner the *target* commits to, not the runner the user's
/// primary workspace currently has on disk.
pub fn run_for_update(
    jj: &JjCli,
    primary_git_dir: &Path,
    workspace_root: &Path,
    cli_runner: Option<Runner>,
    stage: Stage,
    update: &BookmarkUpdate,
    opts: RunOpts,
) -> Result<HookOutcome> {
    // Compute the repo's devenv/direnv env ONCE, eagerly, before any
    // worktree/spawn. The returned Arc is intentionally discarded — the
    // side effect is populating the process-global cache that the spawn
    // sites read via `apply_repo_env`. The opt-outs are read once here.
    let _ = crate::repo_env::repo_env(
        workspace_root,
        crate::repo_env::repo_env_enabled(jj),
        crate::repo_env::repo_env_autoallow_enabled(jj),
    );
    // Record the gate-cache opt-out ONCE here too, beside repo-env, so the
    // spawn sites can point CARGO_TARGET_DIR at the primary `target/`.
    crate::gate_cache::gate_cache(workspace_root, crate::gate_cache::gate_cache_enabled(jj));
    run_for_update_with_cancel(
        jj,
        primary_git_dir,
        workspace_root,
        cli_runner,
        stage,
        update,
        opts,
        &Cancel::never(),
        None,
    )
}

/// Like [`run_for_update`] but takes a cancellation token so callers
/// running multiple updates in parallel can short-circuit siblings
/// when one fails.
///
/// Set the token (`Cancel::cancel`) from a progress callback when
/// any earlier sibling reports `success: false`; the remaining
/// `run_for_update_with_cancel` calls in the same scope will check
/// the token before each subprocess and skip the rest of their
/// pipeline. The function never *kills* an in-flight subprocess —
/// it skips the next one. For an hk config with N steps that
/// translates to "save the (N-1) remaining steps".
#[allow(clippy::too_many_arguments)]
fn run_for_update_with_cancel(
    jj: &JjCli,
    primary_git_dir: &Path,
    workspace_root: &Path,
    cli_runner: Option<Runner>,
    stage: Stage,
    update: &BookmarkUpdate,
    opts: RunOpts,
    cancel: &Cancel,
    warm: Option<&PklWarmCache>,
) -> Result<HookOutcome> {
    let Some(new_commit) = update.new_commit.as_ref() else {
        // Pure delete — nothing to check.
        return Ok(HookOutcome {
            success: true,
            fixup_commit: None,
            retried: false,
            initial_failure: false,
            captured_output: None,
            cancelled: false,
        });
    };

    let diff_base = resolve_from_refs(jj, update)?;
    let setup_steps = setup::load_steps(jj)?;

    let initial = run_once(
        jj,
        primary_git_dir,
        workspace_root,
        cli_runner,
        stage,
        update,
        new_commit,
        &diff_base,
        &setup_steps,
        opts.all_files,
        opts.capture_output,
        cancel,
        warm,
    )?;

    // Initial run was clean OR caller opted out of retry OR there's nothing
    // to retry against — return as-is. (No fixup means the caller's tree is
    // already what the hooks would produce; nothing to re-check.)
    if !opts.retry_after_fixup || initial.success || initial.fixup_commit.is_none() {
        return Ok(HookOutcome {
            success: initial.success,
            fixup_commit: initial.fixup_commit,
            retried: false,
            initial_failure: !initial.success,
            captured_output: initial.captured_output,
            cancelled: initial.cancelled,
        });
    }

    let fixup = initial.fixup_commit.as_ref().expect("checked Some above");
    tracing::info!(
        "{update}: re-running hooks against fixup commit {fixup} to check for transient failure"
    );
    let retry = run_once(
        jj,
        primary_git_dir,
        workspace_root,
        cli_runner,
        stage,
        update,
        fixup,
        &diff_base,
        &setup_steps,
        opts.all_files,
        opts.capture_output,
        cancel,
        warm,
    )?;

    // The retry should be clean (no failure, no new fixup) for the
    // "healed by retry" verdict. Any further fixup means the tree is
    // still drifting; bail with the original failure semantics.
    let healed = retry.success && retry.fixup_commit.is_none();
    // Concatenate initial + retry captured output so the caller sees
    // both passes in order. Only relevant when capture_output is on;
    // when off, both are None.
    let captured_output = match (initial.captured_output, retry.captured_output) {
        (Some(mut a), Some(b)) => {
            a.push_str(&b);
            Some(a)
        }
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    Ok(HookOutcome {
        // If the retry healed it, report success and surface the fixup
        // so the user knows to advance their bookmark. If the retry
        // *also* failed, success is whatever the retry reported and
        // the fixup is whichever one the retry produced (which may
        // differ from the initial one).
        success: if healed { true } else { retry.success },
        fixup_commit: if healed {
            initial.fixup_commit
        } else {
            // The retry pass either produced a fresh fixup (chain of
            // autofixes) or none at all (just a hard failure). Prefer
            // the retry's fixup when it has one so the user advances
            // their bookmark to the most recent good state; fall back
            // to the initial fixup so we don't drop information.
            retry.fixup_commit.or(initial.fixup_commit)
        },
        retried: true,
        initial_failure: true,
        captured_output,
        cancelled: initial.cancelled || retry.cancelled,
    })
}

/// Run `hk validate` in `cwd`, best-effort. `hk validate` evaluates
/// `hk.pkl` — resolving + caching its `package://` imports into the
/// shared `~/.pkl` cache — without running any hook. Output is captured
/// (silent on success); a failure is logged and swallowed — it never aborts
/// the batch (a real config error resurfaces through the per-bookmark run that
/// follows) — and reported as `false` so the caller only marks warmed on success.
fn run_hk_validate(argv: &[String], cwd: &Path, workspace_root: &Path) -> bool {
    tracing::info!("warming hk Pkl cache: {argv:?}");
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(cwd);
    // hk itself may be devenv-pinned; validate under the same env the run
    // will use. JJ_HOOKS_WORKSPACE is set after so it always wins.
    crate::repo_env::apply_repo_env(&mut cmd, workspace_root);
    cmd.env("JJ_HOOKS_WORKSPACE", workspace_root);
    match cmd.output() {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            tracing::debug!(
                "hk Pkl cache warm exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            false
        }
        Err(e) => {
            tracing::debug!("hk Pkl cache warm failed to spawn: {e}");
            false
        }
    }
}

/// Serializes per-worktree Pkl config evaluation across a parallel hook
/// batch so the cold-cache writes never race. hk caches each worktree's
/// resolved config separately, keyed by the config's PATH
/// (`~/.cache/hk/configs/<path-keyed>.json`), so every ephemeral
/// worktree must be warmed individually — deduping on config CONTENT
/// would collapse same-content worktrees (every worktree has the same
/// `hk.pkl` but a unique `/tmp` path) and leave all but one to race.
/// Shared across all per-bookmark threads in one parallel batch; the
/// sequential / single-bookmark paths pass no cache (no concurrency →
/// no race).
#[derive(Default)]
struct PklWarmCache {
    warmed: std::sync::Mutex<std::collections::HashSet<PathBuf>>,
}

impl PklWarmCache {
    /// Warm `worktree`'s cache at most once *successfully*, serialized across
    /// threads so the cold-cache config evaluations never write concurrently.
    /// The lock is held across `validate`, so concurrent callers block until it
    /// returns (and skip an already-warmed worktree).
    ///
    /// Returns `None` when the worktree is warm — already, or `validate` just
    /// succeeded — so the caller's `hk run` may proceed in parallel against the
    /// now-written cache. Returns `Some(guard)` when `validate` failed: the
    /// cache may still be cold, so the caller holds the returned lock guard
    /// across its `hk run`, keeping that cold run serialized against other
    /// workers' warm/run rather than racing the non-atomic cache write.
    #[must_use]
    fn warm_once(
        &self,
        worktree: &Path,
        validate: impl FnOnce() -> bool,
    ) -> Option<std::sync::MutexGuard<'_, std::collections::HashSet<PathBuf>>> {
        let mut warmed = self.warmed.lock().unwrap();
        if warmed.contains(worktree) {
            return None;
        }
        if validate() {
            warmed.insert(worktree.to_path_buf());
            None
        } else {
            Some(warmed)
        }
    }
}

/// Batch entrypoint: run hooks for N bookmark updates in parallel,
/// with fail-fast cancellation across siblings.
///
/// One thread per update. Each thread runs the full
/// [`run_for_update_with_cancel`] pipeline against its own
/// ephemeral worktree — the worktrees are filesystem-isolated and
/// don't share index locks, so per-bookmark hook backends (cargo,
/// hk, etc.) can run truly concurrently. The shared `.git/objects/`
/// directory is read-mostly during hook execution; the per-bookmark
/// `jj git import` invoked at the end of `run_for_update_with_cancel`
/// (if a fixup was produced) relies on jj's own concurrent-op
/// reconciliation.
///
/// Fail-fast: every update in the batch shares one `Cancel` token.
/// As soon as any thread observes `outcome.success == false`, it
/// flips the token; siblings still in the middle of a multi-step
/// hk pipeline check the token between subprocess invocations and
/// short-circuit the rest. For an N-bookmark batch where bookmark
/// 1 fails fmt while bookmarks 2 and 3 are doing clippy, this
/// converts a "wait for two slow clippy runs to finish" symptom
/// into "skip them as soon as the current step exits."
///
/// Use [`run_for_partitioned_updates_parallel`] when the batch
/// represents multiple independent stacks — each stack gets its
/// own Cancel scope so a failure in stack A doesn't cancel stack B.
///
/// Mandatory call-site invariant: `opts.capture_output` MUST be true.
/// Letting N hook backends stream live to the same terminal garbles
/// the user's view. The function asserts on this — passing
/// `capture_output: false` is a programmer error.
///
/// Returns results in the same order as `updates` (not completion
/// order). `progress_start` is invoked once per update on the thread
/// that picks it up, right before any actual work happens (worktree
/// creation, setup steps, hook runner); `progress` is invoked once
/// per update on the thread that finished it. The pair lets the
/// caller render a live spinner / "running" state per bookmark
/// instead of just a post-hoc "passed/failed" line.
///
/// First subprocess error (spawn failure, etc.) aborts before
/// returning; per-update non-zero exits are reported via
/// [`HookOutcome::success`], not as `Err`.
#[allow(clippy::too_many_arguments)]
pub fn run_for_updates_parallel<S, F>(
    jj: &JjCli,
    primary_git_dir: &Path,
    workspace_root: &Path,
    cli_runner: Option<Runner>,
    stage: Stage,
    updates: &[BookmarkUpdate],
    opts: RunOpts,
    progress_start: S,
    progress: F,
) -> Result<Vec<HookOutcome>>
where
    S: Fn(usize, &BookmarkUpdate) + Send + Sync,
    F: Fn(usize, &BookmarkUpdate, &HookOutcome) + Send + Sync,
{
    // Populate the repo-env cache ONCE before the fan-out, so parallel
    // workers read it (never race to compute it). Opt-outs read once here.
    let _ = crate::repo_env::repo_env(
        workspace_root,
        crate::repo_env::repo_env_enabled(jj),
        crate::repo_env::repo_env_autoallow_enabled(jj),
    );
    crate::gate_cache::gate_cache(workspace_root, crate::gate_cache::gate_cache_enabled(jj));
    // One warm cache shared across the batch: the first per-bookmark run
    // for each distinct config validates it (serially) from its own
    // worktree; the rest reuse the now-warm `~/.pkl` cache.
    let warm = PklWarmCache::default();
    let warm = &warm;
    run_updates_parallel_core(
        updates,
        opts.capture_output,
        |_idx, update, cancel| {
            run_for_update_with_cancel(
                jj,
                primary_git_dir,
                workspace_root,
                cli_runner,
                stage,
                update,
                opts,
                cancel,
                Some(warm),
            )
        },
        progress_start,
        progress,
    )
}

/// Orchestration core for [`run_for_updates_parallel`], parameterized
/// over the cache-warm step and the per-update work so the
/// warm-once-before-fan-out invariant is unit-testable without a real
/// git repo. `warm` runs once, serially, before the `thread::scope`
/// fan-out; `work` runs per update on its own thread.
fn run_updates_parallel_core<Work, S, F>(
    updates: &[BookmarkUpdate],
    capture_output: bool,
    work: Work,
    progress_start: S,
    progress: F,
) -> Result<Vec<HookOutcome>>
where
    Work: Fn(usize, &BookmarkUpdate, &Cancel) -> Result<HookOutcome> + Send + Sync,
    S: Fn(usize, &BookmarkUpdate) + Send + Sync,
    F: Fn(usize, &BookmarkUpdate, &HookOutcome) + Send + Sync,
{
    assert!(
        capture_output,
        "run_for_updates_parallel requires capture_output=true; parallel runs without capture garble the terminal",
    );

    use std::sync::Mutex;
    let work = &work;
    let progress_start = &progress_start;
    let progress = &progress;
    let results: Vec<Mutex<Option<Result<HookOutcome>>>> =
        (0..updates.len()).map(|_| Mutex::new(None)).collect();
    let results_ref = &results;
    let cancel = Cancel::new();
    let cancel_ref = &cancel;

    std::thread::scope(|s| {
        for (idx, update) in updates.iter().enumerate() {
            s.spawn(move || {
                progress_start(idx, update);
                let outcome = work(idx, update, cancel_ref);
                if let Ok(o) = &outcome {
                    if !o.success && !o.cancelled {
                        cancel_ref.cancel();
                    }
                    progress(idx, update, o);
                }
                *results_ref[idx].lock().unwrap() = Some(outcome);
            });
        }
    });

    let mut out = Vec::with_capacity(updates.len());
    for slot in results {
        let result = slot
            .into_inner()
            .unwrap()
            .expect("thread::scope joined all threads but a slot is still None");
        out.push(result?);
    }
    Ok(out)
}

/// Partitioned variant of [`run_for_updates_parallel`]. Each
/// partition runs as an atomic fail-fast unit (siblings cancel each
/// other within the partition); partitions are independent (a
/// failure in one partition does NOT cancel any other).
///
/// Use this when the user passed `-b X -b Y` for two unrelated
/// tips: stack X's bookmarks share a Cancel, stack Y's share a
/// different Cancel, and stack Y keeps going to completion even
/// if stack X fails out on its first bookmark.
///
/// Partitions run concurrently with each other (each partition is
/// its own `run_for_updates_parallel` call inside a `thread::scope`).
/// Outcomes are returned in the same shape as the input partitions
/// (`Vec<Vec<HookOutcome>>`), in the same order.
///
/// `progress_start` is called as `(partition_idx, update_idx_in_partition,
/// update)` when the thread begins work on a bookmark; `progress` is
/// called as `(partition_idx, update_idx_in_partition, update, outcome)`
/// when the thread finishes one update.
#[allow(clippy::too_many_arguments)]
pub fn run_for_partitioned_updates_parallel<S, F>(
    jj: &JjCli,
    primary_git_dir: &Path,
    workspace_root: &Path,
    cli_runner: Option<Runner>,
    stage: Stage,
    partitions: &[Vec<BookmarkUpdate>],
    opts: RunOpts,
    progress_start: S,
    progress: F,
) -> Result<Vec<Vec<HookOutcome>>>
where
    S: Fn(usize, usize, &BookmarkUpdate) + Send + Sync,
    F: Fn(usize, usize, &BookmarkUpdate, &HookOutcome) + Send + Sync,
{
    // Populate the repo-env cache ONCE before the fan-out (see
    // `run_for_updates_parallel`). Opt-outs read once here.
    let _ = crate::repo_env::repo_env(
        workspace_root,
        crate::repo_env::repo_env_enabled(jj),
        crate::repo_env::repo_env_autoallow_enabled(jj),
    );
    crate::gate_cache::gate_cache(workspace_root, crate::gate_cache::gate_cache_enabled(jj));
    // One warm cache shared across all partitions (see
    // `run_for_updates_parallel`): each distinct config is warmed once,
    // serially, from its own target worktree.
    let warm = PklWarmCache::default();
    let warm = &warm;
    run_partitioned_updates_parallel_core(
        partitions,
        opts.capture_output,
        |_p_idx, _u_idx, update, cancel| {
            run_for_update_with_cancel(
                jj,
                primary_git_dir,
                workspace_root,
                cli_runner,
                stage,
                update,
                opts,
                cancel,
                Some(warm),
            )
        },
        progress_start,
        progress,
    )
}

/// Orchestration core for [`run_for_partitioned_updates_parallel`].
/// Mirrors [`run_updates_parallel_core`] but keeps the per-partition
/// `Cancel` scoping: `warm` fires once before the fan-out; each
/// partition still gets its own cancellation token.
fn run_partitioned_updates_parallel_core<Work, S, F>(
    partitions: &[Vec<BookmarkUpdate>],
    capture_output: bool,
    work: Work,
    progress_start: S,
    progress: F,
) -> Result<Vec<Vec<HookOutcome>>>
where
    Work: Fn(usize, usize, &BookmarkUpdate, &Cancel) -> Result<HookOutcome> + Send + Sync,
    S: Fn(usize, usize, &BookmarkUpdate) + Send + Sync,
    F: Fn(usize, usize, &BookmarkUpdate, &HookOutcome) + Send + Sync,
{
    assert!(
        capture_output,
        "run_for_partitioned_updates_parallel requires capture_output=true",
    );

    use std::sync::Mutex;
    let work = &work;
    let progress_start = &progress_start;
    let progress = &progress;
    let results: Vec<Vec<Mutex<Option<Result<HookOutcome>>>>> = partitions
        .iter()
        .map(|p| (0..p.len()).map(|_| Mutex::new(None)).collect())
        .collect();
    let results_ref = &results;

    std::thread::scope(|s| {
        for (p_idx, partition) in partitions.iter().enumerate() {
            let cancel = Cancel::new();
            for (u_idx, update) in partition.iter().enumerate() {
                let cancel = cancel.clone();
                s.spawn(move || {
                    progress_start(p_idx, u_idx, update);
                    let outcome = work(p_idx, u_idx, update, &cancel);
                    if let Ok(o) = &outcome {
                        if !o.success && !o.cancelled {
                            cancel.cancel();
                        }
                        progress(p_idx, u_idx, update, o);
                    }
                    *results_ref[p_idx][u_idx].lock().unwrap() = Some(outcome);
                });
            }
        }
    });

    let mut out = Vec::with_capacity(partitions.len());
    for partition_slots in results {
        let mut partition_out = Vec::with_capacity(partition_slots.len());
        for slot in partition_slots {
            let result = slot
                .into_inner()
                .unwrap()
                .expect("thread::scope joined but a slot is still None");
            partition_out.push(result?);
        }
        out.push(partition_out);
    }
    Ok(out)
}

/// Sequential counterpart to [`run_for_updates_parallel`] for the
/// `--hooks-sequential` opt-out path. Same contract (per-bookmark
/// `run_for_update`, in-order results) but no thread fan-out. Output
/// streams live by default — `opts.capture_output` is honored if
/// set, but unlike the parallel variant it isn't required.
#[allow(clippy::too_many_arguments)]
pub fn run_for_updates_sequential<F>(
    jj: &JjCli,
    primary_git_dir: &Path,
    workspace_root: &Path,
    cli_runner: Option<Runner>,
    stage: Stage,
    updates: &[BookmarkUpdate],
    opts: RunOpts,
    progress: F,
) -> Result<Vec<HookOutcome>>
where
    F: Fn(usize, &BookmarkUpdate, &HookOutcome),
{
    let mut out = Vec::with_capacity(updates.len());
    for (idx, update) in updates.iter().enumerate() {
        let outcome = run_for_update(
            jj,
            primary_git_dir,
            workspace_root,
            cli_runner,
            stage,
            update,
            opts,
        )?;
        progress(idx, update, &outcome);
        out.push(outcome);
    }
    Ok(out)
}

/// Internal shape returned by [`run_once`]: a single hook run plus the
/// fixup commit (if any) it produced. This is the per-attempt building
/// block used by [`run_for_update`] to layer retry-after-fixup logic.
struct OnceOutcome {
    success: bool,
    fixup_commit: Option<String>,
    /// `Some(buf)` iff the caller asked for capture (see
    /// [`RunOpts::capture_output`]). Carries the concatenated
    /// stdout+stderr of every subprocess invoked during this pass.
    captured_output: Option<String>,
    /// `true` iff this pass short-circuited because the cancellation
    /// token was already set when the pass started. Distinguishes
    /// "cancelled before doing real work" from "ran to completion
    /// and happened to succeed" so the result-collection layer can
    /// filter cancelled outcomes out of progress callbacks.
    cancelled: bool,
}

/// Replace element 0 of `command_argv` (the bare runner binary name
/// produced by `hook_command{,_all_files}` / `lefthook_command{,_all_files}`)
/// with the resolved argv prefix from [`crate::runner::resolve_runner_argv`].
///
/// For the common case the prefix is a single element (an absolute path
/// or just the bare name found on $PATH), so this is a near-no-op. For
/// the `uv run --` wrapper case the prefix is multiple elements; we
/// drop the placeholder name and splice in the wrapper.
fn splice_runner_prefix(prefix: &[String], command_argv: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(prefix.len() + command_argv.len().saturating_sub(1));
    out.extend(prefix.iter().cloned());
    if command_argv.len() > 1 {
        out.extend(command_argv[1..].iter().cloned());
    }
    out
}

/// One pass through the hook pipeline against a specific target commit.
///
/// Builds a fresh worktree at `target_commit`, runs the hook backend
/// against each entry in `from_refs`, and, if the worktree's tree
/// differs from `target_commit`'s tree at the end, builds a fixup
/// commit + cleans up the temp ref / bookmark that `jj git import`
/// creates.
///
/// When `capture_output` is true, every subprocess's stdout+stderr
/// gets folded into the returned `OnceOutcome::captured_output`
/// instead of streaming to the parent terminal. The trade is no live
/// progress bar — the caller (typically [`run_for_updates_parallel`])
/// replays the captured block when the pass finishes.
///
/// Callers (currently [`run_for_update`] and the batch entrypoint)
/// decide whether to retry based on the returned `success` /
/// `fixup_commit`.
#[allow(clippy::too_many_arguments)]
fn run_once(
    jj: &JjCli,
    primary_git_dir: &Path,
    workspace_root: &Path,
    cli_runner: Option<Runner>,
    stage: Stage,
    update: &BookmarkUpdate,
    target_commit: &str,
    diff_base: &DiffBase,
    setup_steps: &[SetupStep],
    all_files: bool,
    capture_output: bool,
    cancel: &Cancel,
    warm: Option<&PklWarmCache>,
) -> Result<OnceOutcome> {
    if cancel.is_cancelled() {
        return Ok(OnceOutcome {
            success: true,
            fixup_commit: None,
            captured_output: None,
            cancelled: true,
        });
    }
    let wt = Worktree::create(primary_git_dir, target_commit)?;

    // User-declared setup commands (e.g. `bun install`) run inside
    // the worktree before the runner so hooks have install-time
    // resources (`node_modules`, `.venv`, etc.) available. A
    // non-zero exit aborts before the runner is invoked — the
    // worktree is unhealthy and there's no point asking the
    // runner to grade it.
    //
    // Output is always captured: silent on success (the daily
    // case: `bun install` chattering about which packages it
    // installed is noise nobody wants), included in the captured
    // buffer on failure or when `capture_output` is on for the
    // whole pass.
    //
    // A failure is converted into a `success: false` OnceOutcome
    // rather than propagated as a hard error so the parallel
    // runner can still classify other bookmarks per-partition.
    // The captured setup output rides along on the `captured_output`
    // field so it shows up in the same dump the user already sees
    // for a hook failure.
    let setup_captured = match setup::run_steps(setup_steps, wt.path(), workspace_root) {
        Ok(captured) => captured,
        Err(JjHooksError::SetupFailed {
            name,
            status,
            captured,
        }) => {
            // Same buffer shape the hook-failure path produces: the
            // captured stdout/stderr plus a trailing line explaining
            // *why* the buffer ends here.
            let mut buf = captured;
            if !buf.ends_with('\n') {
                buf.push('\n');
            }
            buf.push_str(&format!(
                "setup step `{name}` exited with status {status}; \
                 skipping hook runner for this bookmark\n",
            ));
            return Ok(OnceOutcome {
                success: false,
                fixup_commit: None,
                captured_output: Some(buf),
                cancelled: false,
            });
        }
        Err(other) => return Err(other),
    };

    // Resolve the runner from the target commit's tree, not the primary
    // workspace. `--runner` overrides; otherwise autodetect against the
    // worktree we just checked out. If autodetect comes up empty, the
    // commit doesn't have a hook config — silent-skip with an info log.
    let runner = match cli_runner {
        Some(r) => r,
        None => {
            let Some(r) = Runner::autodetect(wt.path())? else {
                eprintln!(
                    "jj-hooks: {update}: no hook-runner config in target commit; skipping hooks"
                );
                return Ok(OnceOutcome {
                    success: true,
                    fixup_commit: None,
                    captured_output: None,
                    cancelled: false,
                });
            };
            // prek is a faster drop-in for pre-commit; prefer it when
            // present. The override path already skips this so an explicit
            // `--runner pre-commit` keeps the slower binary.
            //
            // "Present" here means resolvable through any of the layers
            // in [`resolve_runner_argv`], not just $PATH — a prek
            // installed only inside a venv (the issue #17 scenario) is
            // still preferable to the pre-commit on $PATH if the user
            // bothered to `prek install` the shim or set the config.
            let prek_present = crate::runner::resolve_runner_argv(
                Runner::Prek,
                jj,
                workspace_root,
                primary_git_dir,
                stage,
            )
            .is_ok();
            crate::runner::prefer_prek_when_available(r, prek_present)
        }
    };

    // Pre-check that the runner binary is on PATH. Without this, the
    // `Command::status()` call below surfaces a libc-level
    // `posix_spawn: No such file or directory (os error 2)` with no
    // indication of *which* binary couldn't be found. The common case
    // for prek users is that prek is installed only inside a Python
    // venv — jj-hooks runs in a clean ephemeral worktree and doesn't
    // inherit the venv's PATH, so the user sees the cryptic error
    // and has no idea it was prek that was missing.
    //
    // Resolution order is (1) explicit config, (2) the path baked into
    // the `.git/hooks/<stage>` shim by `prek install` / `pre-commit
    // install`, (3) `uv run` when uv.lock + uv are both present,
    // (4) plain $PATH. See `resolve_runner_argv` for details.
    let runner_argv =
        crate::runner::resolve_runner_argv(runner, jj, workspace_root, primary_git_dir, stage)?;

    // Warm hk's config cache for THIS worktree before the runner runs.
    // hk caches each worktree's resolved config separately, keyed by the
    // config's path; on a cold cache the parallel per-bookmark
    // evaluations race the (non-atomic) cache writes and abort with a
    // nondeterministic `field not found` error. `warm` (present only on
    // the parallel paths) runs a serial `hk validate` per worktree, so
    // every concurrent `hk run` reads an already-written cache.
    // Best-effort — a real error resurfaces through the run below. When the
    // warm fails the cache may still be cold, so `warm_once` hands back the
    // lock guard; holding it through this worker's run (to fn end) keeps that
    // cold run serialized against other workers instead of racing the write.
    let _warm_guard = match warm {
        Some(warm) if runner == Runner::Hk => {
            let validate_argv =
                splice_runner_prefix(&runner_argv, &crate::runner::hk_validate_command());
            warm.warm_once(wt.path(), || {
                run_hk_validate(&validate_argv, wt.path(), workspace_root)
            })
        }
        _ => None,
    };

    // all_files: ignore the diff range and run each runner's
    // "lint every tracked file" command exactly once. The from-refs
    // are meaningless here — the runner sees no --from-ref/--to-ref.
    // This fires either when the caller requested all-files mode
    // (`jj-hp run --all-files`) or when the diff base resolved to
    // `DiffBase::AllFiles` (first push of a root-parented commit, #284).
    //
    // Default path: iterate the resolved from-refs (one per ancestor on
    // the remote) so multi-ancestor pushes still get the full set of
    // diff bases. Each iteration accumulates modifications in the
    // shared worktree, mirroring how the standard pre-push pipeline
    // builds up its fixup.
    let all_files = all_files || matches!(diff_base, DiffBase::AllFiles);
    let from_refs: &[String] = match diff_base {
        DiffBase::Refs(refs) => refs,
        DiffBase::AllFiles => &[],
    };
    let mut success = true;
    // Seed the captured buffer with the setup-step output when the
    // caller asked us to capture. Setup output is always captured
    // inside `run_steps` (it has to be, to attach it to a
    // `SetupFailed` error), so it's already in hand here — we just
    // decide whether to fold it into the per-bookmark buffer the
    // caller will see on `--verbose` / failure.
    let mut captured = if capture_output {
        Some(setup_captured)
    } else {
        None
    };
    let mut cancelled = false;
    if all_files {
        if cancel.is_cancelled() {
            cancelled = true;
        } else {
            let argv = match runner {
                Runner::Lefthook => lefthook_command_all_files(stage),
                _ => hook_command_all_files(runner, stage),
            };
            let argv = splice_runner_prefix(&runner_argv, &argv);
            tracing::info!("running (--all-files): {:?}", argv);
            let ok = run_subprocess(&argv, wt.path(), workspace_root, captured.as_mut())?;
            if !ok {
                success = false;
            }
        }
    } else {
        for from_ref in from_refs {
            // Cancellation check between subprocess invocations. The
            // hk/cargo subprocess itself isn't cancellable from
            // outside, but skipping the *next* one short-circuits
            // the rest of this bookmark's pipeline. For a typical
            // hk config (fmt → clippy-native → clippy-wasm) this
            // saves ~30-60s on cold caches when a parallel sibling
            // bookmark already failed.
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }
            let argv = match runner {
                Runner::Lefthook => {
                    let files = changed_files(wt.path(), from_ref, target_commit)?;
                    lefthook_command(stage, &files)
                }
                _ => hook_command(runner, stage, from_ref, target_commit),
            };
            let argv = splice_runner_prefix(&runner_argv, &argv);

            tracing::info!("running: {:?}", argv);
            let ok = run_subprocess(&argv, wt.path(), workspace_root, captured.as_mut())?;
            if !ok {
                success = false;
            }
        }
    }

    let fixup_commit =
        maybe_build_fixup_commit(primary_git_dir, wt.path(), target_commit, &update.bookmark)?;

    if fixup_commit.is_some() {
        // Make jj aware of the new commit. --ignore-working-copy keeps
        // this import from racing against any concurrent `jj` process
        // (same lock-contention rationale as in push.rs).
        jj.run(&["git", "import", "--ignore-working-copy"])?;

        // jj git import created a `jj-hooks-fixup/<bookmark>` jj bookmark
        // from the underlying refs/heads/jj-hooks-fixup/<bookmark> ref.
        // Clean both up immediately — the user almost always wants to
        // either squash the fixup into the parent or move their bookmark
        // forward themselves, not have a stale temp bookmark lying
        // around. The commit stays addressable by hash via `jj log`,
        // `jj show`, `jj squash --from <hash>` etc. since jj tracks it
        // in its own commit graph independent of the ref.
        let temp_bookmark = fixup_bookmark(&update.bookmark);
        // `jj bookmark forget` removes the jj bookmark, but in a
        // secondary workspace it leaves the underlying refs/heads/<name>
        // ref alive in the primary's git dir. Explicitly delete the
        // git ref ourselves so the cleanup is uniform.
        let _ = jj.run(&[
            "bookmark",
            "forget",
            &temp_bookmark,
            "--ignore-working-copy",
        ]);
        let _ = delete_git_ref(primary_git_dir, &fixup_ref(&update.bookmark));
    }

    Ok(OnceOutcome {
        success,
        fixup_commit,
        captured_output: captured,
        cancelled,
    })
}

/// Run a hook subprocess. When `capture` is `Some`, the child's
/// stdout+stderr are captured into the buffer (chronological order is
/// approximated by concatenating stdout then stderr — the runner CLIs
/// we wrap mostly print failures to stderr so this preserves the
/// signal even though it's not a true byte-level interleave). When
/// `capture` is `None`, the child inherits stdio so the user sees the
/// runner's progress bar live.
///
/// Returns `Ok(true)` on a zero exit, `Ok(false)` on any non-zero
/// exit. IO errors (spawn failure, etc.) propagate as `Err`.
fn run_subprocess(
    argv: &[String],
    cwd: &Path,
    workspace_root: &Path,
    capture: Option<&mut String>,
) -> Result<bool> {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(cwd);
    // Merge the repo's direnv/devenv env (once-computed, cached) BEFORE
    // JJ_HOOKS_WORKSPACE so that variable always wins. No-op unless a batch
    // entrypoint populated the cache for this workspace_root.
    crate::repo_env::apply_repo_env(&mut cmd, workspace_root);
    // Point CARGO_TARGET_DIR at the primary `target/` AFTER apply_repo_env so
    // a repo-env-carried value can never win. No-op unless enabled.
    crate::gate_cache::apply_gate_cache(&mut cmd, workspace_root);
    cmd.env("JJ_HOOKS_WORKSPACE", workspace_root);
    match capture {
        None => {
            let status = cmd.status()?;
            Ok(status.success())
        }
        Some(buf) => {
            let output = cmd.output()?;
            // Tag the captured block with the argv so the user can
            // see which subprocess produced each chunk when N hook
            // backends are multiplexed.
            buf.push_str(&format!("$ {}\n", argv.join(" ")));
            buf.push_str(&String::from_utf8_lossy(&output.stdout));
            if !output.stderr.is_empty() {
                buf.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            if !buf.ends_with('\n') {
                buf.push('\n');
            }
            Ok(output.status.success())
        }
    }
}

/// The diff base(s) a hook backend should grade a bookmark update
/// against.
enum DiffBase {
    /// One or more concrete commits to use as `--from-ref`. Each entry
    /// is diffed against the target commit in turn (multi-ancestor
    /// pushes get one base per already-on-remote ancestor).
    Refs(Vec<String>),
    /// Grade every tracked file as newly added — each backend's native
    /// all-files mode, no `--from-ref` at all.
    ///
    /// This arm exists for the first push of a root-parented commit
    /// (#284): its only parent is jj's synthetic root commit, which has
    /// no git object, so `{new}^` is an unresolvable ref. There is no
    /// well-known empty-*commit* hash to stand in (commits embed
    /// author/date, so no fixed value exists). Git's empty-*tree* hash
    /// works as a base for the git-diff backends (pre-commit, lefthook),
    /// but hk resolves the base through libgit2 `merge_base`, which
    /// rejects a tree object — so no single ref serves all three
    /// backends. All-files mode is the semantically correct answer
    /// anyway: a root-parented first push adds every file.
    AllFiles,
}

/// Resolve the diff base for a bookmark update. For an existing bookmark
/// update we just use the old commit; for a new bookmark we find the
/// heads of `::new & ::remote_bookmarks(remote)` so each already-on-remote
/// ancestor becomes its own diff base. A new bookmark whose only parent
/// is the jj root commit resolves to [`DiffBase::AllFiles`] (see #284).
fn resolve_from_refs(jj: &JjCli, update: &BookmarkUpdate) -> Result<DiffBase> {
    if let Some(old) = update.old_commit.as_ref() {
        return Ok(DiffBase::Refs(vec![old.clone()]));
    }

    let new = update.new_commit.as_ref().expect("not a delete here");
    let revset = format!(
        "heads(::{new} & ::remote_bookmarks(remote=exact:{}))",
        update.remote
    );

    let template = r#"commit_id ++ "\n""#;
    let out = jj.run(&[
        "log",
        "--no-graph",
        "-r",
        &revset,
        "-T",
        template,
        "--ignore-working-copy",
    ])?;

    let refs: Vec<String> = out
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect();

    if refs.is_empty() {
        // New bookmark on a totally fresh remote — no ancestors on the
        // remote at all. Normally we diff against the parent of `new`
        // (`{new}^`). But when `new`'s only parent is jj's synthetic
        // root commit, `{new}^` is not a resolvable git ref (the root
        // has no git object), so hk/pre-commit crash before any hook
        // runs (#284). In that case grade every file as added via the
        // backend's all-files mode instead.
        //
        // `parents(new) ~ root()` is empty iff every parent of `new`
        // is the root commit. Verified against the pinned jj version.
        let root_parent_revset = format!("parents({new}) ~ root()");
        let parents_out = jj.run(&[
            "log",
            "--no-graph",
            "-r",
            &root_parent_revset,
            "-T",
            template,
            "--ignore-working-copy",
        ])?;
        let has_real_parent = parents_out.lines().any(|l| !l.trim().is_empty());
        if has_real_parent {
            return Ok(DiffBase::Refs(vec![format!("{new}^")]));
        }
        return Ok(DiffBase::AllFiles);
    }

    Ok(DiffBase::Refs(refs))
}

fn changed_files(worktree: &Path, from: &str, to: &str) -> Result<Vec<PathBuf>> {
    let out = Command::new("git")
        .args(["diff", "--name-only", "--diff-filter=ACMR"])
        .arg(format!("{from}..{to}"))
        .current_dir(worktree)
        .output()?;
    if !out.status.success() {
        return Err(JjHooksError::JjFailed {
            status: out.status.code().unwrap_or(-1),
            stderr: format!(
                "git diff --name-only failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| PathBuf::from(l.trim()))
        .filter(|p| !p.as_os_str().is_empty())
        .collect())
}

/// Stage everything in the worktree, hash the resulting tree, and
/// compare against the parent commit's tree. Returns a fixup commit
/// only when the trees actually differ — `git status --porcelain`
/// can report a worktree as dirty (e.g. when a hook runner touched
/// the index without changing file content; hk's auto-stage path
/// does this even on check-only steps), but the resulting tree is
/// often identical to the parent and an empty fixup commit is just
/// noise that pins the bookmark to a content-equivalent revision
/// and aborts the push.
///
/// Content-addressed gating eliminates the false positive: if the
/// hooks didn't actually change any file, the write-tree OID equals
/// the parent's tree OID and we return `None`.
fn maybe_build_fixup_commit(
    primary_git_dir: &Path,
    worktree: &Path,
    parent: &str,
    bookmark: &str,
) -> Result<Option<String>> {
    // Stage everything (tracked + untracked) and hash the tree.
    // Both are cheap on a clean checkout — `git add -A` is a no-op
    // when nothing changed; `git write-tree` is hashing-only.
    run_git(worktree, &["add", "-A"])?;
    let tree = run_git_capture(worktree, &["write-tree"])?;

    // Parent's tree as a content reference. `<commit>^{tree}` is
    // the standard rev-parse spelling.
    let parent_tree_spec = format!("{parent}^{{tree}}");
    let parent_tree = run_git_capture(worktree, &["rev-parse", &parent_tree_spec])?;

    if tree == parent_tree {
        return Ok(None);
    }

    // Build the commit object via the *primary* git dir so the resulting
    // commit lives in the shared object database.
    let message = format!("jj-hooks: autofixes for {bookmark}");
    let commit = run_git_capture_with_git_dir(
        primary_git_dir,
        worktree,
        &["commit-tree", &tree, "-p", parent, "-m", &message],
    )?;

    // Anchor under refs/heads/ so `jj git import` will pick it up as a
    // bookmark. (Refs outside refs/heads/ and refs/remotes/ are invisible
    // to jj's git import logic.)
    let ref_name = fixup_ref(bookmark);
    run_git_capture_with_git_dir(
        primary_git_dir,
        worktree,
        &["update-ref", &ref_name, &commit],
    )?;

    Ok(Some(commit))
}

/// The git ref where a fixup commit gets anchored for a given bookmark.
/// Lives under `refs/heads/` so `jj git import` picks it up as a bookmark.
pub fn fixup_ref(bookmark: &str) -> String {
    format!("refs/heads/jj-hooks-fixup/{}", sanitize_for_ref(bookmark))
}

/// The jj bookmark name corresponding to `fixup_ref`.
pub fn fixup_bookmark(bookmark: &str) -> String {
    format!("jj-hooks-fixup/{}", sanitize_for_ref(bookmark))
}

/// Replace characters that git rejects in ref names (per git-check-ref-format)
/// with `_`. Real bookmark names like `main` or `feature/foo` pass through
/// unchanged; synthesized names like `revset:@` (used by `jj-hp run @`) get
/// scrubbed so the resulting `refs/heads/jj-hooks-fixup/<name>` is valid.
fn sanitize_for_ref(s: &str) -> String {
    // Per-character offenders first; then collapse multi-char sequences
    // and trim the position-sensitive ones (leading `-`/`.`, trailing
    // `.`/`.lock`/`/`, internal `//`).
    let mut out: String = s
        .chars()
        .map(|c| match c {
            ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\' | '\x7f' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();

    while out.contains("..") {
        out = out.replace("..", "__");
    }
    while out.contains("@{") {
        out = out.replace("@{", "@_");
    }
    if out.starts_with('-') {
        out.replace_range(0..1, "_");
    }
    if out.starts_with('.') {
        out.replace_range(0..1, "_");
    }
    if out.ends_with('.') {
        let n = out.len();
        out.replace_range(n - 1..n, "_");
    }
    if out.ends_with(".lock") {
        let n = out.len();
        out.replace_range(n - 5..n - 4, "_");
    }
    if out.ends_with('/') {
        let n = out.len();
        out.replace_range(n - 1..n, "_");
    }
    while out.contains("//") {
        out = out.replace("//", "/_");
    }
    if out.is_empty() {
        return "_".into();
    }
    out
}

/// Delete a git ref in the given git dir, ignoring "ref doesn't exist"
/// failures. Used to clean up the temp `refs/heads/jj-hooks-fixup/<name>`
/// after `jj git import` + `jj bookmark forget` from a secondary
/// workspace (where forget leaves the underlying ref alive).
fn delete_git_ref(git_dir: &Path, ref_name: &str) -> Result<()> {
    let out = Command::new("git")
        .arg(format!("--git-dir={}", git_dir.display()))
        .args(["update-ref", "-d", ref_name])
        .output()?;
    if !out.status.success() {
        // Treat any failure as best-effort: if the ref didn't exist,
        // that's the desired state already.
        tracing::debug!(
            "git update-ref -d {ref_name} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git").args(args).current_dir(cwd).output()?;
    if !out.status.success() {
        return Err(JjHooksError::JjFailed {
            status: out.status.code().unwrap_or(-1),
            stderr: format!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        });
    }
    Ok(())
}

fn run_git_capture(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").args(args).current_dir(cwd).output()?;
    if !out.status.success() {
        return Err(JjHooksError::JjFailed {
            status: out.status.code().unwrap_or(-1),
            stderr: format!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn run_git_capture_with_git_dir(git_dir: &Path, cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg(format!("--git-dir={}", git_dir.display()))
        .args(args)
        .current_dir(cwd)
        .output()?;
    if !out.status.success() {
        return Err(JjHooksError::JjFailed {
            status: out.status.code().unwrap_or(-1),
            stderr: format!(
                "git --git-dir={} {args:?} failed: {}",
                git_dir.display(),
                String::from_utf8_lossy(&out.stderr)
            ),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn fixup_ref_for_plain_bookmark() {
        assert_eq!(fixup_ref("main"), "refs/heads/jj-hooks-fixup/main");
    }

    #[test]
    fn fixup_ref_keeps_internal_slash() {
        // jj bookmark names commonly contain `/` (e.g. `feature/foo`) and
        // git accepts them as path separators inside a ref.
        assert_eq!(
            fixup_ref("feature/foo"),
            "refs/heads/jj-hooks-fixup/feature/foo"
        );
    }

    #[test]
    fn fixup_ref_scrubs_colon() {
        // The bug from issue #1: `jj-hp run @` synthesizes `revset:@`.
        // Without sanitization, git rejects the ref with "bad name".
        assert_eq!(fixup_ref("revset:@"), "refs/heads/jj-hooks-fixup/revset_@");
    }

    #[test]
    fn sanitize_replaces_each_invalid_char() {
        // One probe per character class git-check-ref-format rejects.
        assert_eq!(sanitize_for_ref("a:b"), "a_b");
        assert_eq!(sanitize_for_ref("a~b"), "a_b");
        assert_eq!(sanitize_for_ref("a^b"), "a_b");
        assert_eq!(sanitize_for_ref("a?b"), "a_b");
        assert_eq!(sanitize_for_ref("a*b"), "a_b");
        assert_eq!(sanitize_for_ref("a[b"), "a_b");
        assert_eq!(sanitize_for_ref("a\\b"), "a_b");
        assert_eq!(sanitize_for_ref("a b"), "a_b");
        assert_eq!(sanitize_for_ref("a\tb"), "a_b");
        assert_eq!(sanitize_for_ref("a\x7fb"), "a_b");
    }

    #[test]
    fn sanitize_collapses_double_dot() {
        assert_eq!(sanitize_for_ref("a..b"), "a__b");
        // `..` replacement is non-overlapping: `a...b` becomes `a__.b`
        // (first `..` matches at positions 1-2 and gets replaced; the
        // remaining `.` is harmless mid-string).
        assert_eq!(sanitize_for_ref("a...b"), "a__.b");
        assert!(!sanitize_for_ref("a....b").contains(".."));
    }

    #[test]
    fn sanitize_collapses_at_brace() {
        assert_eq!(sanitize_for_ref("a@{b"), "a@_b");
    }

    #[test]
    fn sanitize_strips_leading_dash() {
        assert_eq!(sanitize_for_ref("-foo"), "_foo");
    }

    #[test]
    fn sanitize_strips_leading_dot() {
        assert_eq!(sanitize_for_ref(".foo"), "_foo");
    }

    #[test]
    fn sanitize_strips_trailing_dot() {
        assert_eq!(sanitize_for_ref("foo."), "foo_");
    }

    #[test]
    fn sanitize_strips_trailing_dot_lock() {
        assert_eq!(sanitize_for_ref("foo.lock"), "foo_lock");
    }

    #[test]
    fn sanitize_strips_trailing_slash() {
        assert_eq!(sanitize_for_ref("foo/"), "foo_");
    }

    #[test]
    fn sanitize_collapses_double_slash() {
        assert_eq!(sanitize_for_ref("a//b"), "a/_b");
    }

    #[test]
    fn sanitize_empty_becomes_underscore() {
        // Defensive: if the input is empty after some external transform,
        // emit a single underscore so the joined ref isn't dangling.
        assert_eq!(sanitize_for_ref(""), "_");
    }

    #[test]
    fn fixup_bookmark_uses_same_sanitizer() {
        // fixup_bookmark feeds `jj bookmark forget` which is also strict
        // about colon (jj rejects bookmark names with `:` in them).
        assert_eq!(fixup_bookmark("revset:@"), "jj-hooks-fixup/revset_@");
    }

    // PklWarmCache serializes the per-worktree cold-cache config writes
    // and warms each worktree once (keyed by PATH — hk's config cache is
    // path-keyed, so same-content worktrees must each be warmed). The
    // cold-cache race itself is timing-dependent and is deliberately not
    // reproduced (a non-hermetic repro would be flaky); these guard the
    // invariants the fix relies on.

    #[test]
    fn pkl_warm_cache_dedups_same_worktree() {
        let cache = PklWarmCache::default();
        let count = AtomicUsize::new(0);
        let wt = Path::new("/tmp/wt-a");
        for _ in 0..3 {
            let _ = cache.warm_once(wt, || {
                count.fetch_add(1, Ordering::SeqCst);
                true
            });
        }
        assert_eq!(count.load(Ordering::SeqCst), 1, "same worktree warms once");
    }

    #[test]
    fn pkl_warm_cache_warms_each_distinct_worktree() {
        // Same-content worktrees at distinct paths must EACH warm: hk's
        // config cache is path-keyed, so deduping by content would leave
        // all but one cold.
        let cache = PklWarmCache::default();
        let count = AtomicUsize::new(0);
        for p in ["/tmp/wt-a", "/tmp/wt-b", "/tmp/wt-c"] {
            let _ = cache.warm_once(Path::new(p), || {
                count.fetch_add(1, Ordering::SeqCst);
                true
            });
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            3,
            "each distinct worktree warms once"
        );
    }

    #[test]
    fn pkl_warm_cache_same_worktree_validates_once_under_concurrency() {
        let cache = PklWarmCache::default();
        let count = AtomicUsize::new(0);
        let wt = Path::new("/tmp/wt-x");
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    let _ = cache.warm_once(wt, || {
                        count.fetch_add(1, Ordering::SeqCst);
                        true
                    });
                });
            }
        });
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "concurrent callers warm a worktree once"
        );
    }

    #[test]
    fn pkl_warm_cache_serializes_distinct_worktrees() {
        // Distinct worktrees each warm once, and never two validates at a
        // time — the per-worktree writes must be serial (the whole point
        // of the lock).
        let cache = PklWarmCache::default();
        let total = AtomicUsize::new(0);
        let active = AtomicUsize::new(0);
        let max_active = AtomicUsize::new(0);
        // Capture shared references (Copy) so each scoped thread borrows
        // the same atomics + cache rather than moving them.
        let (cache, total, active, max_active) = (&cache, &total, &active, &max_active);
        let paths: Vec<PathBuf> = (0..8)
            .map(|i| PathBuf::from(format!("/tmp/wt-{i}")))
            .collect();
        std::thread::scope(|s| {
            for p in &paths {
                s.spawn(move || {
                    let _ = cache.warm_once(p, || {
                        let cur = active.fetch_add(1, Ordering::SeqCst) + 1;
                        max_active.fetch_max(cur, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        total.fetch_add(1, Ordering::SeqCst);
                        active.fetch_sub(1, Ordering::SeqCst);
                        true
                    });
                });
            }
        });
        assert_eq!(
            total.load(Ordering::SeqCst),
            8,
            "each distinct worktree warms once"
        );
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            1,
            "writes must be serialized"
        );
    }

    #[test]
    fn pkl_warm_cache_failed_validate_retries_then_caches() {
        // A failed `validate` (returns false) must NOT mark the worktree
        // warmed, so the next caller retries rather than running hooks on an
        // unwarmed cache; once one succeeds, further callers skip.
        let cache = PklWarmCache::default();
        let count = AtomicUsize::new(0);
        let wt = Path::new("/tmp/wt-fail");
        for _ in 0..2 {
            let _ = cache.warm_once(wt, || {
                count.fetch_add(1, Ordering::SeqCst);
                false
            });
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "a failed validate must not mark the worktree warmed"
        );
        let _ = cache.warm_once(wt, || {
            count.fetch_add(1, Ordering::SeqCst);
            true
        });
        let _ = cache.warm_once(wt, || {
            count.fetch_add(1, Ordering::SeqCst);
            true
        });
        assert_eq!(
            count.load(Ordering::SeqCst),
            3,
            "once warmed, further callers skip validate"
        );
    }

    #[test]
    fn pkl_warm_cache_failed_warm_returns_guard_for_serialized_run() {
        // The fix: a failed warm hands back the lock guard so the caller holds
        // it across its cold-cache `hk run`, serializing that run against other
        // workers instead of racing the non-atomic cache write. A successful
        // (or already-warm) warm returns None so runs proceed in parallel.
        let cache = PklWarmCache::default();
        assert!(
            cache
                .warm_once(Path::new("/tmp/wt-fail"), || false)
                .is_some(),
            "failed warm returns the lock guard to serialize the cold run"
        );
        assert!(
            cache.warm_once(Path::new("/tmp/wt-ok"), || true).is_none(),
            "successful warm returns None — run may proceed in parallel"
        );
        let wt = Path::new("/tmp/wt-warm");
        let _ = cache.warm_once(wt, || true);
        assert!(
            cache
                .warm_once(wt, || panic!("must not re-validate"))
                .is_none(),
            "already-warm worktree returns None without re-validating"
        );
    }

    #[test]
    fn run_subprocess_applies_repo_env_patch() {
        // A seeded PATH-setting patch must reach the child, and
        // JJ_HOOKS_WORKSPACE (set AFTER the patch) must still win.
        let ws = tempfile::TempDir::new().unwrap();
        crate::repo_env::test_seed(
            ws.path(),
            crate::repo_env::EnvPatch::Patch(std::collections::HashMap::from([(
                "REPO_ENV_MARKER".to_string(),
                Some("applied".to_string()),
            )])),
        );
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf 'MARK=[%s]' \"${REPO_ENV_MARKER:-unset}\"".to_string(),
        ];
        let mut buf = String::new();
        let ok = run_subprocess(&argv, ws.path(), ws.path(), Some(&mut buf)).unwrap();
        assert!(ok);
        // RED before T2: run_subprocess set only JJ_HOOKS_WORKSPACE, so the
        // child saw `unset` and this assertion failed.
        assert!(buf.contains("MARK=[applied]"), "captured: {buf:?}");
    }

    #[test]
    fn run_subprocess_strips_git_local_env_from_patch() {
        // A patch that (wrongly) carries GIT_DIR must NOT reach the hook
        // child — it would make the child's git resolve against the primary
        // workspace instead of the temp worktree.
        let ws = tempfile::TempDir::new().unwrap();
        crate::repo_env::test_seed(
            ws.path(),
            crate::repo_env::EnvPatch::Patch(std::collections::HashMap::from([(
                "GIT_DIR".to_string(),
                Some("/bogus/primary/.git".to_string()),
            )])),
        );
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf 'GD=[%s]' \"${GIT_DIR:-unset}\"".to_string(),
        ];
        let mut buf = String::new();
        run_subprocess(&argv, ws.path(), ws.path(), Some(&mut buf)).unwrap();
        assert!(buf.contains("GD=[unset]"), "captured: {buf:?}");
    }

    #[test]
    fn run_subprocess_strips_inherited_git_local_env_on_disabled_path() {
        // #292 end-to-end at the spawn site: a non-direnv repo (EnvPatch::Disabled)
        // pushed from a linked worktree inherits GIT_DIR from jj-hp's own env.
        // The strip must fire on the Disabled arm too, so the hook child sees it
        // unset. GIT_DIR is set on the process env (not a patch) to model the
        // inheritance vector; nextest runs each test in its own process, so the
        // mutation is isolated.
        let ws = tempfile::TempDir::new().unwrap();
        crate::repo_env::test_seed(ws.path(), crate::repo_env::EnvPatch::Disabled);
        // SAFETY: nextest process-per-test isolation (cf. repo_env_real.rs).
        unsafe {
            std::env::set_var("GIT_DIR", "/bogus/primary/.git");
        }
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf 'GD=[%s]' \"${GIT_DIR:-unset}\"".to_string(),
        ];
        let mut buf = String::new();
        run_subprocess(&argv, ws.path(), ws.path(), Some(&mut buf)).unwrap();
        unsafe {
            std::env::remove_var("GIT_DIR");
        }
        assert!(
            buf.contains("GD=[unset]"),
            "disabled-path spawn must strip inherited GIT_DIR; captured: {buf:?}"
        );
    }

    #[test]
    fn run_subprocess_no_patch_is_unchanged() {
        // Non-regression: with no cache entry, the child env is the parent's
        // plus JJ_HOOKS_WORKSPACE — no devenv patch applied. (The git
        // repo-location strip always runs but is a no-op here: no git-local
        // var is set on this command's env.)
        let ws = tempfile::TempDir::new().unwrap();
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf 'WS=[%s]' \"${JJ_HOOKS_WORKSPACE:-unset}\"".to_string(),
        ];
        let mut buf = String::new();
        run_subprocess(&argv, ws.path(), ws.path(), Some(&mut buf)).unwrap();
        assert!(
            buf.contains(&format!("WS=[{}]", ws.path().display())),
            "captured: {buf:?}"
        );
    }
}
