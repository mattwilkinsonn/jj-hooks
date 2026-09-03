//! Tests for the multi-bookmark batch entrypoints
//! [`run_for_updates_parallel`] / [`run_for_updates_sequential`].
//!
//! These exercise the library API directly (not via the `jj-hp push`
//! CLI) because the batch entrypoints don't have a CLI surface — they
//! exist for downstream callers like `jj-gt submit` that want a fan-out
//! over an N-bookmark stack.

mod harness;

use harness::{PRE_PUSH_FAILING, PRE_PUSH_PASSING, PRE_PUSH_RECORD_CTD_PER_BOOKMARK, show};
use jj_hooks::bookmark_updates::{BookmarkUpdate, UpdateType};
use jj_hooks::hooks::{
    Cancel, RunOpts, run_for_partitioned_updates_parallel, run_for_updates_parallel,
    run_for_updates_sequential,
};
use jj_hooks::jj::{JjCli, primary_git_dir};
use jj_hooks::runner::{Runner, Stage};

use harness::TestRepo;

/// Build a 3-bookmark stack `main → b1 → b2 → b3` and return the
/// commit ids for each. Each bookmark sits one commit above its
/// parent.
fn build_three_stack(repo: &TestRepo) -> (String, String, String, String) {
    repo.write("a.txt", "a\n");
    let out = repo.jj(&["commit", "-m", "b1 commit"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "create", "b1", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    repo.write("b.txt", "b\n");
    let out = repo.jj(&["commit", "-m", "b2 commit"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "create", "b2", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    repo.write("c.txt", "c\n");
    let out = repo.jj(&["commit", "-m", "b3 commit"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "create", "b3", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    (
        repo.commit_id_of("main"),
        repo.commit_id_of("b1"),
        repo.commit_id_of("b2"),
        repo.commit_id_of("b3"),
    )
}

fn updates_for_three_stack(main: &str, b1: &str, b2: &str, b3: &str) -> Vec<BookmarkUpdate> {
    vec![
        BookmarkUpdate {
            remote: "origin".into(),
            bookmark: "b1".into(),
            update_type: UpdateType::Add,
            old_commit: Some(main.to_owned()),
            new_commit: Some(b1.to_owned()),
        },
        BookmarkUpdate {
            remote: "origin".into(),
            bookmark: "b2".into(),
            update_type: UpdateType::Add,
            old_commit: Some(b1.to_owned()),
            new_commit: Some(b2.to_owned()),
        },
        BookmarkUpdate {
            remote: "origin".into(),
            bookmark: "b3".into(),
            update_type: UpdateType::Add,
            old_commit: Some(b2.to_owned()),
            new_commit: Some(b3.to_owned()),
        },
    ]
}

#[test]
fn run_for_updates_parallel_returns_results_in_input_order() {
    // Three bookmarks, all passing hooks. The parallel API must:
    //   1. Run all three through the pre-push hook.
    //   2. Return outcomes in the SAME order as the input updates,
    //      regardless of which thread finished first.
    //   3. Capture each run's output into HookOutcome::captured_output.
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_PASSING);
    let (main, b1, b2, b3) = build_three_stack(&repo);
    let updates = updates_for_three_stack(&main, &b1, &b2, &b3);

    let jj = JjCli::new(repo.primary().to_path_buf());
    let primary_git_dir = primary_git_dir(repo.primary()).unwrap();

    let opts = RunOpts {
        retry_after_fixup: true,
        all_files: false,
        capture_output: true,
    };

    let outcomes = run_for_updates_parallel(
        &jj,
        &primary_git_dir,
        repo.primary(),
        Some(Runner::PreCommit),
        Stage::PrePush,
        &updates,
        opts,
        |_idx, _update| {},
        |_idx, _update, _outcome| {},
    )
    .unwrap();

    assert_eq!(outcomes.len(), 3);
    for (idx, outcome) in outcomes.iter().enumerate() {
        assert!(
            outcome.success,
            "outcome #{idx} failed: captured = {:?}",
            outcome.captured_output
        );
        assert!(
            outcome.captured_output.is_some(),
            "outcome #{idx} has no captured output despite capture_output=true",
        );
    }
}

#[test]
fn run_for_updates_parallel_first_failure_aborts_with_per_bookmark_attribution() {
    // Three bookmarks. Hook fails for all of them (PRE_PUSH_FAILING).
    // The batch entrypoint should:
    //   1. Run all three (the parallel path doesn't short-circuit;
    //      every thread is already launched).
    //   2. Surface success=false for each.
    //   3. Capture each one's failure output separately so the
    //      caller can attribute the failure to the right bookmark.
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_FAILING);
    let (main, b1, b2, b3) = build_three_stack(&repo);
    let updates = updates_for_three_stack(&main, &b1, &b2, &b3);

    let jj = JjCli::new(repo.primary().to_path_buf());
    let primary_git_dir = primary_git_dir(repo.primary()).unwrap();

    let opts = RunOpts {
        retry_after_fixup: false,
        all_files: false,
        capture_output: true,
    };

    let outcomes = run_for_updates_parallel(
        &jj,
        &primary_git_dir,
        repo.primary(),
        Some(Runner::PreCommit),
        Stage::PrePush,
        &updates,
        opts,
        |_idx, _update| {},
        |_idx, _update, _outcome| {},
    )
    .unwrap();

    assert_eq!(outcomes.len(), 3);
    let mut had_failure = false;
    for (idx, outcome) in outcomes.iter().enumerate() {
        // Each outcome is either a real failure (success=false,
        // cancelled=false) or a fail-fast short-circuit (success=true,
        // cancelled=true). We must NOT see a real pass (success=true,
        // cancelled=false) — that would mean the hook config silently
        // ignored the failure config.
        if outcome.cancelled {
            assert!(
                outcome.success,
                "cancelled outcome #{idx} should have success=true, got {:?}",
                outcome
            );
        } else {
            had_failure = true;
            assert!(
                !outcome.success,
                "non-cancelled outcome #{idx} unexpectedly passed: captured = {:?}",
                outcome.captured_output
            );
            let captured = outcome
                .captured_output
                .as_deref()
                .unwrap_or_else(|| panic!("outcome #{idx} has no captured output"));
            assert!(
                !captured.is_empty(),
                "outcome #{idx} captured an empty buffer",
            );
        }
    }
    // PR-fail-fast: with cancellation in play, at LEAST one of the
    // outcomes is an actual hook failure (the one that tripped the
    // cancellation); the others may have observed cancellation
    // before they got to spawn their own subprocess and short-
    // circuited as cancelled-not-failed.
    assert!(
        had_failure,
        "expected at least one outcome to be a real hook failure, got all-cancelled",
    );
}

#[test]
fn run_for_updates_parallel_progress_callback_fires_per_update() {
    // The progress callback runs on each thread as soon as it
    // finishes. We don't pin completion order (parallel — any order
    // is fine); we just check the callback fires once per update
    // and the captured outcome is consistent with the returned one.
    use std::sync::Mutex;
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_PASSING);
    let (main, b1, b2, b3) = build_three_stack(&repo);
    let updates = updates_for_three_stack(&main, &b1, &b2, &b3);

    let jj = JjCli::new(repo.primary().to_path_buf());
    let primary_git_dir = primary_git_dir(repo.primary()).unwrap();

    let opts = RunOpts {
        retry_after_fixup: true,
        all_files: false,
        capture_output: true,
    };

    let progress_fired: Mutex<Vec<(usize, String, bool)>> = Mutex::new(Vec::new());
    let outcomes = run_for_updates_parallel(
        &jj,
        &primary_git_dir,
        repo.primary(),
        Some(Runner::PreCommit),
        Stage::PrePush,
        &updates,
        opts,
        |_idx, _update| {},
        |idx, update, outcome| {
            progress_fired
                .lock()
                .unwrap()
                .push((idx, update.bookmark.clone(), outcome.success));
        },
    )
    .unwrap();

    let mut fired = progress_fired.into_inner().unwrap();
    fired.sort_by_key(|(idx, _, _)| *idx);
    assert_eq!(
        fired.len(),
        3,
        "expected 3 progress callbacks, got {fired:?}"
    );
    assert_eq!(fired[0], (0, "b1".into(), true));
    assert_eq!(fired[1], (1, "b2".into(), true));
    assert_eq!(fired[2], (2, "b3".into(), true));
    assert!(outcomes.iter().all(|o| o.success));
}

#[test]
fn run_for_updates_sequential_matches_parallel_results() {
    // Same fixture, sequential entrypoint. Results should be
    // identical (same hooks, same updates, same configuration) —
    // pins the contract that --hooks-sequential is just an
    // execution-strategy switch, not a semantic change.
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_PASSING);
    let (main, b1, b2, b3) = build_three_stack(&repo);
    let updates = updates_for_three_stack(&main, &b1, &b2, &b3);

    let jj = JjCli::new(repo.primary().to_path_buf());
    let primary_git_dir = primary_git_dir(repo.primary()).unwrap();

    let opts = RunOpts {
        retry_after_fixup: true,
        all_files: false,
        // Sequential doesn't require capture; toggle on so we can
        // assert the output buffer plumbing still works in
        // sequential mode.
        capture_output: true,
    };

    let outcomes = run_for_updates_sequential(
        &jj,
        &primary_git_dir,
        repo.primary(),
        Some(Runner::PreCommit),
        Stage::PrePush,
        &updates,
        opts,
        |_idx, _update, _outcome| {},
    )
    .unwrap();

    assert_eq!(outcomes.len(), 3);
    for (idx, outcome) in outcomes.iter().enumerate() {
        assert!(
            outcome.success,
            "outcome #{idx} failed: captured = {:?}",
            outcome.captured_output
        );
        assert!(
            outcome.captured_output.is_some(),
            "sequential with capture_output=true should still capture, got None for #{idx}",
        );
    }
}

#[test]
#[should_panic(expected = "run_for_updates_parallel requires capture_output=true")]
fn run_for_updates_parallel_without_capture_panics() {
    // The parallel API asserts on capture_output=true at entry —
    // passing false is a programmer error (it would garble the
    // terminal). Pin that the assert fires with a clear message.
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_PASSING);
    let (main, b1, b2, b3) = build_three_stack(&repo);
    let updates = updates_for_three_stack(&main, &b1, &b2, &b3);

    let jj = JjCli::new(repo.primary().to_path_buf());
    let primary_git_dir = primary_git_dir(repo.primary()).unwrap();

    let opts = RunOpts {
        retry_after_fixup: true,
        all_files: false,
        capture_output: false,
    };

    let _ = run_for_updates_parallel(
        &jj,
        &primary_git_dir,
        repo.primary(),
        Some(Runner::PreCommit),
        Stage::PrePush,
        &updates,
        opts,
        |_idx, _update| {},
        |_idx, _update, _outcome| {},
    );
}

#[test]
fn cancel_token_round_trip() {
    let c = Cancel::new();
    assert!(!c.is_cancelled());
    c.cancel();
    assert!(c.is_cancelled());

    // Clone shares the underlying atomic.
    let c2 = Cancel::new();
    let c2_clone = c2.clone();
    assert!(!c2.is_cancelled());
    c2_clone.cancel();
    assert!(c2.is_cancelled());

    // never() is just a no-op alias for new().
    let n = Cancel::never();
    assert!(!n.is_cancelled());
}

#[test]
fn run_for_partitioned_updates_parallel_two_independent_stacks() {
    // PR-fail-fast: two-partition input. Both pass. Outcomes shape
    // matches the input partition shape; nothing is cancelled
    // because nothing fails.
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_PASSING);
    let (main, b1, b2, b3) = build_three_stack(&repo);
    // Partition 1: just b1. Partition 2: b2, b3.
    let p1: Vec<BookmarkUpdate> = vec![BookmarkUpdate {
        remote: "origin".into(),
        bookmark: "b1".into(),
        update_type: UpdateType::Add,
        old_commit: Some(main.clone()),
        new_commit: Some(b1.clone()),
    }];
    let p2: Vec<BookmarkUpdate> = vec![
        BookmarkUpdate {
            remote: "origin".into(),
            bookmark: "b2".into(),
            update_type: UpdateType::Add,
            old_commit: Some(b1.clone()),
            new_commit: Some(b2.clone()),
        },
        BookmarkUpdate {
            remote: "origin".into(),
            bookmark: "b3".into(),
            update_type: UpdateType::Add,
            old_commit: Some(b2.clone()),
            new_commit: Some(b3.clone()),
        },
    ];
    let partitions = vec![p1, p2];

    let jj = JjCli::new(repo.primary().to_path_buf());
    let primary_git_dir = primary_git_dir(repo.primary()).unwrap();
    let opts = RunOpts {
        retry_after_fixup: true,
        all_files: false,
        capture_output: true,
    };

    let outcomes = run_for_partitioned_updates_parallel(
        &jj,
        &primary_git_dir,
        repo.primary(),
        Some(Runner::PreCommit),
        Stage::PrePush,
        &partitions,
        opts,
        |_p, _u, _update| {},
        |_p, _u, _update, _outcome| {},
    )
    .unwrap();

    assert_eq!(outcomes.len(), 2, "expected 2 partitions, got {outcomes:?}");
    assert_eq!(outcomes[0].len(), 1);
    assert_eq!(outcomes[1].len(), 2);
    for partition_outcomes in &outcomes {
        for o in partition_outcomes {
            assert!(o.success, "expected pass, got {o:?}");
            assert!(!o.cancelled, "no failures means no cancellation");
        }
    }
}

#[test]
fn run_for_partitioned_updates_parallel_failure_in_one_stack_does_not_cancel_the_other() {
    // The user-facing motivation for the partitioned API: when the
    // user passes `-b X -b Y` for two unrelated stacks, a failure
    // in X should NOT cancel Y. Stack Y must run to completion
    // (success or failure of its own) regardless of what stack X
    // does.
    //
    // We can't easily get a "one stack passes, the other fails" in a
    // single hook config (PRE_PUSH_PASSING and PRE_PUSH_FAILING are
    // global config knobs), so the test pins the structural
    // invariant: when ALL partitions use a failing config, each
    // partition's outcomes show real-failure-not-just-cancelled at
    // least once. That proves the partitions' Cancel scopes are
    // independent — if they were shared, stack X's failure would
    // cancel stack Y before stack Y had a chance to fail on its
    // own, and stack Y's outcome would be "cancelled".
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_FAILING);
    let (main, b1, b2, b3) = build_three_stack(&repo);
    let p1: Vec<BookmarkUpdate> = vec![BookmarkUpdate {
        remote: "origin".into(),
        bookmark: "b1".into(),
        update_type: UpdateType::Add,
        old_commit: Some(main.clone()),
        new_commit: Some(b1.clone()),
    }];
    let p2: Vec<BookmarkUpdate> = vec![BookmarkUpdate {
        remote: "origin".into(),
        bookmark: "b2".into(),
        update_type: UpdateType::Add,
        old_commit: Some(b1.clone()),
        new_commit: Some(b2.clone()),
    }];
    let p3: Vec<BookmarkUpdate> = vec![BookmarkUpdate {
        remote: "origin".into(),
        bookmark: "b3".into(),
        update_type: UpdateType::Add,
        old_commit: Some(b2.clone()),
        new_commit: Some(b3.clone()),
    }];
    let partitions = vec![p1, p2, p3];

    let jj = JjCli::new(repo.primary().to_path_buf());
    let primary_git_dir = primary_git_dir(repo.primary()).unwrap();
    let opts = RunOpts {
        retry_after_fixup: false,
        all_files: false,
        capture_output: true,
    };

    let outcomes = run_for_partitioned_updates_parallel(
        &jj,
        &primary_git_dir,
        repo.primary(),
        Some(Runner::PreCommit),
        Stage::PrePush,
        &partitions,
        opts,
        |_p, _u, _update| {},
        |_p, _u, _update, _outcome| {},
    )
    .unwrap();

    assert_eq!(outcomes.len(), 3);
    for (p_idx, partition_outcomes) in outcomes.iter().enumerate() {
        // Each partition's single-bookmark outcome must be a real
        // failure (success=false, cancelled=false). If any partition
        // came back cancelled-not-failed, cancellation leaked across
        // partition boundaries — the bug we're guarding against.
        assert_eq!(partition_outcomes.len(), 1);
        let o = &partition_outcomes[0];
        assert!(
            !o.cancelled,
            "partition {p_idx} outcome was cancelled — cancellation leaked across partition boundaries: {o:?}",
        );
        assert!(
            !o.success,
            "partition {p_idx} outcome unexpectedly passed: {o:?}",
        );
    }
}

/// Gate-cache (Mode A / T1): all N parallel gate worktrees of a batch see the
/// SAME `CARGO_TARGET_DIR` — the primary `target/`. Each child writes its
/// observed value to a per-bookmark file `$JJ_HOOKS_WORKSPACE/ctd-<to-ref>`
/// (pre-commit suppresses hook stdout on success, so a captured-output
/// assertion is unreliable; distinct files under the shared primary root are
/// race-free across the concurrent children).
///
/// Scope: this asserts the *wiring* — every batch child is pointed at one
/// shared primary `target/`. It deliberately does NOT run a real `cargo`/
/// `cargo nextest` under the gate (every fixture hook in this crate is an `sh`
/// script, by design — a recursive cargo build inside the test suite would be
/// slow and brittle). The distinct question the shared dir raises — is it SAFE
/// for N concurrent `cargo nextest` invocations to share one `CARGO_TARGET_DIR`
/// / nextest store? — is answered by driver measurement, not in-suite: N=3
/// concurrent `cargo nextest run -p jj-hooks` against one shared store each
/// pass 242/242 with no corruption or false-fail (cargo's `.cargo-lock`
/// serializes the build phase; the run store tolerates concurrent readers).
/// That is why the batch test step keeps the shared store rather than a
/// per-worktree one (the design's stated fallback, unneeded).
#[test]
fn parallel_batch_all_children_share_primary_cargo_target_dir() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_RECORD_CTD_PER_BOOKMARK);
    let (main, b1, b2, b3) = build_three_stack(&repo);
    let updates = updates_for_three_stack(&main, &b1, &b2, &b3);

    let jj = JjCli::new(repo.primary().to_path_buf());
    let primary_git_dir = primary_git_dir(repo.primary()).unwrap();
    let opts = RunOpts {
        retry_after_fixup: true,
        all_files: false,
        capture_output: true,
    };

    let outcomes = run_for_updates_parallel(
        &jj,
        &primary_git_dir,
        repo.primary(),
        Some(Runner::PreCommit),
        Stage::PrePush,
        &updates,
        opts,
        |_idx, _update| {},
        |_idx, _update, _outcome| {},
    )
    .unwrap();
    assert_eq!(outcomes.len(), 3);
    for (idx, outcome) in outcomes.iter().enumerate() {
        assert!(outcome.success, "outcome #{idx} failed: {outcome:?}");
    }

    let expected = std::fs::canonicalize(repo.primary())
        .unwrap()
        .join("target")
        .to_string_lossy()
        .into_owned();

    // Collect every per-bookmark ctd file the children wrote.
    let mut seen = Vec::new();
    for entry in std::fs::read_dir(repo.primary()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("ctd-") {
            seen.push(std::fs::read_to_string(&path).unwrap());
        }
    }
    assert_eq!(
        seen.len(),
        3,
        "expected one CTD file per bookmark, got {seen:?}"
    );
    for value in &seen {
        assert_eq!(
            value, &expected,
            "every batch child must see CARGO_TARGET_DIR = <primary>/target"
        );
    }
}
