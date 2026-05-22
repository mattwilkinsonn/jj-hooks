//! End-to-end integration tests for the push pipeline.
//!
//! Each test sets up a fresh primary jj+git repo with a local bare remote,
//! writes a pre-commit config, makes a change, and exercises `jj-hooks`
//! against it.

mod harness;

use harness::{
    HK_PRE_PUSH_AUTOFIX, HK_PRE_PUSH_FAILING, HK_PRE_PUSH_PASSING, LEFTHOOK_PRE_PUSH_AUTOFIX,
    LEFTHOOK_PRE_PUSH_FAILING, LEFTHOOK_PRE_PUSH_PASSING, PRE_PUSH_AUTOFIX, PRE_PUSH_FAILING,
    PRE_PUSH_INDEX_TOUCH_ONLY, PRE_PUSH_PASSING, TestRepo, show,
};

/// Sanity: harness builds a working primary + remote and `jj git push` works
/// directly. If this fails the rest of the tests are noise.
#[test]
fn harness_smoke() {
    let repo = TestRepo::new();

    // Make a new change that moves `main` forward.
    repo.write("hello.txt", "hello\n");
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let out = repo.jj(&["git", "push", "-b", "main", "--dry-run"]);
    assert!(out.status.success(), "{}", show(&out));
}

#[test]
fn no_runner_config_passes_through_to_jj_git_push() {
    let repo = TestRepo::new();

    // Move main forward on a new change.
    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let head = repo.commit_id_of("main");

    // No .pre-commit-config.yaml → should fall through to jj git push.
    let out = repo.jj_hooks(&["push", "-b", "main"]);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(repo.remote_commit("main").as_deref(), Some(head.as_str()));
}

#[test]
fn delete_only_push_skips_hooks() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_FAILING);

    // Create a throwaway bookmark on the initial commit, push it, then delete it.
    let out = repo.jj(&["bookmark", "create", "tmp", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["git", "push", "-b", "tmp", "--allow-new"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "delete", "tmp"]);
    assert!(out.status.success(), "{}", show(&out));

    // Delete-only push should skip hooks even though config says "always fail".
    let out = repo.jj_hooks(&["--runner", "pre-commit", "push", "-b", "tmp"]);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(repo.remote_commit("tmp"), None);
}

#[test]
fn passing_hooks_pushes() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_PASSING);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let head = repo.commit_id_of("main");

    let out = repo.jj_hooks(&["--runner", "pre-commit", "push", "-b", "main"]);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(repo.remote_commit("main").as_deref(), Some(head.as_str()));
}

#[test]
fn failing_hooks_abort_push() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_FAILING);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let remote_before = repo.remote_commit("main");

    let out = repo.jj_hooks(&["--runner", "pre-commit", "push", "-b", "main"]);
    assert!(
        !out.status.success(),
        "expected nonzero exit:\n{}",
        show(&out)
    );

    // Remote should not have moved.
    assert_eq!(repo.remote_commit("main"), remote_before);
    // No fixup ref should exist either (hook didn't modify anything).
    assert!(repo.refs_matching("refs/jj-hooks/fixup/*").is_empty());
}

#[test]
fn hook_autofix_creates_fixup_ref_and_aborts_push() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_AUTOFIX);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let remote_before = repo.remote_commit("main");
    let local_main_before = repo.commit_id_of("main");

    let out = repo.jj_hooks(&["--runner", "pre-commit", "push", "-b", "main"]);
    assert!(
        !out.status.success(),
        "expected nonzero exit:\n{}",
        show(&out)
    );

    // Remote did not move.
    assert_eq!(repo.remote_commit("main"), remote_before);
    // The temp git ref + jj bookmark should both be gone after
    // post-import cleanup. The fixup commit itself stays addressable
    // by hash via jj_knows_commit.
    assert!(
        repo.rev_parse("refs/heads/jj-hooks-fixup/main").is_none(),
        "temp fixup ref should be cleaned up after import"
    );
    let fixup = repo
        .fixup_commit_for("main")
        .expect("fixup commit should be findable by description");
    assert!(
        repo.jj_knows_commit(&fixup),
        "jj should still see the fixup commit even with no ref pointing at it"
    );
    // Local bookmark should not have advanced (default behavior).
    assert_eq!(repo.commit_id_of("main"), local_main_before);
}

#[test]
fn hook_autofix_with_advance_bookmarks_moves_local_bookmark() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_AUTOFIX);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let local_main_before = repo.commit_id_of("main");

    let out = repo.jj_hooks(&[
        "--runner",
        "pre-commit",
        "push",
        "--advance-bookmarks",
        "-b",
        "main",
    ]);
    assert!(
        !out.status.success(),
        "expected nonzero exit:\n{}",
        show(&out)
    );

    let local_main_after = repo.commit_id_of("main");
    assert_ne!(
        local_main_after, local_main_before,
        "bookmark should have moved to the fixup commit"
    );

    // The fixup commit's parent should be the previous main commit.
    let parent = repo.commit_id_of(&format!("{local_main_after}-"));
    assert_eq!(parent, local_main_before);

    // The temporary jj-hooks-fixup/main bookmark and its ref should both be
    // gone after the advance — the real bookmark is the anchor now.
    assert!(
        repo.rev_parse("refs/heads/jj-hooks-fixup/main").is_none(),
        "temporary fixup ref should be cleaned up after --advance-bookmarks"
    );
}

/// Issue #7 regression: a hook that touches the index without
/// changing file content (e.g. a runner's stash + restore
/// lifecycle that leaves the index stat-mismatched against the
/// final on-disk content) must NOT produce an empty fixup commit.
///
/// pre-commit additionally detects "files were modified by this
/// hook" mid-flight and reports the hook as failed, so the push
/// still aborts — but the abort path's "hooks modified files
/// (fixup commit …)" branch must not fire, because the resulting
/// tree is identical to the parent.
///
/// Pre-fix shape: `worktree_dirty` returns true because
/// `git status --porcelain` reports an index-stat change; we
/// commit an empty fixup commit. The push log shows BOTH "hook
/// failed" AND "hooks modified files (fixup commit …)" — the
/// second line is the bug.
///
/// Post-fix shape: tree comparison sees parent's tree == current
/// tree → no fixup commit emitted. Push still aborts because the
/// hook itself returned non-zero.
#[test]
fn index_touch_without_content_change_does_not_emit_fixup() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_INDEX_TOUCH_ONLY);

    // Tracked file the hook will round-trip. Commit before the hook
    // runs so it's part of the parent's tree.
    repo.write("existing.txt", "stable content\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let remote_before = repo.remote_commit("main");

    let out = repo.jj_hooks(&["--runner", "pre-commit", "push", "-b", "main"]);
    // The push aborts because the hook failed.
    assert!(
        !out.status.success(),
        "push should abort when the hook reports failure:\n{}",
        show(&out)
    );

    // Remote should not have moved.
    assert_eq!(repo.remote_commit("main"), remote_before);

    // No empty fixup commit should have been emitted, because the
    // resulting tree was identical to the parent's tree.
    assert!(
        repo.rev_parse("refs/heads/jj-hooks-fixup/main").is_none(),
        "no fixup ref should be created when the worktree's tree is unchanged"
    );
    assert!(
        repo.fixup_commit_for("main").is_none(),
        "no fixup commit should be addressable when the worktree's tree is unchanged"
    );
}

#[test]
fn hook_autofix_from_secondary_workspace() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_AUTOFIX);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let secondary = repo.add_secondary("secondary");

    let out = repo.jj_hooks_in(
        &secondary,
        &["--runner", "pre-commit", "push", "-b", "main"],
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !stderr.contains("Your pre-commit configuration is unstaged"),
        "secondary workspace should not trigger pre-commit unstaged warning:\n{stderr}"
    );
    assert!(!out.status.success(), "{}", show(&out));

    // Temp ref + jj bookmark should be gone post-import. Fixup commit
    // itself stays addressable by hash.
    assert!(
        repo.rev_parse("refs/heads/jj-hooks-fixup/main").is_none(),
        "temp fixup ref should be cleaned up after import"
    );
    let fixup = repo
        .fixup_commit_for("main")
        .expect("fixup commit should be findable by description");
    assert!(repo.jj_knows_commit(&fixup));
}

#[test]
fn new_bookmark_uses_remote_ancestors_resolution() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_PASSING);

    // Make a new commit and create a brand new bookmark on it.
    repo.write("feature.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "feature"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "create", "feature", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let head = repo.commit_id_of("feature");

    let out = repo.jj_hooks(&[
        "--runner",
        "pre-commit",
        "push",
        "-b",
        "feature",
        "--allow-new",
    ]);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(
        repo.remote_commit("feature").as_deref(),
        Some(head.as_str())
    );
}

#[test]
fn multi_bookmark_one_fail_blocks_all() {
    let repo = TestRepo::new();

    // Make a config that fails (so any push with hooks will fail).
    repo.write_pre_commit_config(PRE_PUSH_FAILING);

    // Move main forward.
    repo.write("a.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "main move"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    // Create a feature bookmark too.
    repo.write("b.txt", "y\n");
    let out = repo.jj(&["commit", "-m", "feature commit"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "create", "feature", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let main_remote_before = repo.remote_commit("main");
    let feature_remote_before = repo.remote_commit("feature");

    let out = repo.jj_hooks(&[
        "--runner",
        "pre-commit",
        "push",
        "-b",
        "main",
        "-b",
        "feature",
        "--allow-new",
    ]);
    assert!(!out.status.success(), "{}", show(&out));

    assert_eq!(repo.remote_commit("main"), main_remote_before);
    assert_eq!(repo.remote_commit("feature"), feature_remote_before);
}

#[test]
fn run_subcommand_executes_hooks_without_pushing() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_PASSING);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));

    let remote_before = repo.remote_commit("main");

    let out = repo.jj_hooks(&["--runner", "pre-commit", "run", "--stage", "pre-push", "@-"]);
    assert!(out.status.success(), "{}", show(&out));
    // Remote unchanged — run does not push.
    assert_eq!(repo.remote_commit("main"), remote_before);
}

#[test]
fn run_subcommand_autofix_does_not_crash_on_bad_ref_name() {
    // Regression for issue #1: `jj-hp run <revset>` synthesizes a bookmark
    // name of `revset:<revset>` and feeds it through `fixup_ref`, which
    // produced `refs/heads/jj-hooks-fixup/revset:@` — a name git rejects
    // ("refusing to update ref with bad name"). The sanitizer scrubs `:`
    // and friends so the ref is well-formed.
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_AUTOFIX);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));

    // `jj-hp run` exits nonzero when hooks modified files (same contract
    // as the push path). The bug we're guarding against is a crash with
    // status 128 / "bad name", not a clean nonzero exit. Check the
    // stderr explicitly so a regression of the original symptom fails
    // loudly even if the exit code happens to match.
    let out = repo.jj_hooks(&["--runner", "pre-commit", "run", "--stage", "pre-push", "@-"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("refusing to update ref with bad name"),
        "expected sanitizer to scrub `:` from synthesized ref:\n{}",
        show(&out)
    );
    assert!(
        !stderr.contains("update_ref failed"),
        "ref update should succeed after sanitization:\n{}",
        show(&out)
    );

    // The synthesized bookmark name `revset:@-` becomes `revset_@-` for
    // the fixup ref. Confirm the sanitized ref was created (and then
    // cleaned up post-import, same as the regular push path).
    assert!(
        repo.rev_parse("refs/heads/jj-hooks-fixup/revset_@-")
            .is_none(),
        "temp fixup ref should be cleaned up after import"
    );
    let fixup = repo
        .fixup_commit_for("revset:@-")
        .expect("fixup commit should be findable by description");
    assert!(repo.jj_knows_commit(&fixup));
}

// -- prek -------------------------------------------------------------------
//
// prek is CLI-compatible with pre-commit and reads the same
// .pre-commit-config.yaml, so we reuse the existing fixtures. These three
// tests + the pre-commit ones above cover the runners' identical CLI
// surface from two different binaries.

#[test]
fn prek_passing_hooks_pushes() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_PASSING);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let head = repo.commit_id_of("main");

    let out = repo.jj_hooks(&["--runner", "prek", "push", "-b", "main"]);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(repo.remote_commit("main").as_deref(), Some(head.as_str()));
}

#[test]
fn prek_failing_hooks_abort_push() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_FAILING);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let remote_before = repo.remote_commit("main");

    let out = repo.jj_hooks(&["--runner", "prek", "push", "-b", "main"]);
    assert!(!out.status.success(), "{}", show(&out));
    assert_eq!(repo.remote_commit("main"), remote_before);
}

#[test]
fn prek_hook_autofix_creates_fixup_ref() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_AUTOFIX);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let out = repo.jj_hooks(&["--runner", "prek", "push", "-b", "main"]);
    assert!(!out.status.success(), "{}", show(&out));
    assert!(
        repo.rev_parse("refs/heads/jj-hooks-fixup/main").is_none(),
        "temp fixup ref should be cleaned up after import"
    );
    let fixup = repo
        .fixup_commit_for("main")
        .expect("fixup commit should be findable by description");
    assert!(repo.jj_knows_commit(&fixup));
}

// -- lefthook ---------------------------------------------------------------

#[test]
fn lefthook_passing_hooks_pushes() {
    let repo = TestRepo::new();
    repo.write_lefthook_config(LEFTHOOK_PRE_PUSH_PASSING);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let head = repo.commit_id_of("main");

    let out = repo.jj_hooks(&["push", "-b", "main"]);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(repo.remote_commit("main").as_deref(), Some(head.as_str()));
}

#[test]
fn lefthook_failing_hooks_abort_push() {
    let repo = TestRepo::new();
    repo.write_lefthook_config(LEFTHOOK_PRE_PUSH_FAILING);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let remote_before = repo.remote_commit("main");

    let out = repo.jj_hooks(&["push", "-b", "main"]);
    assert!(!out.status.success(), "{}", show(&out));
    assert_eq!(repo.remote_commit("main"), remote_before);
}

#[test]
fn lefthook_hook_autofix_creates_fixup_ref() {
    let repo = TestRepo::new();
    repo.write_lefthook_config(LEFTHOOK_PRE_PUSH_AUTOFIX);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let out = repo.jj_hooks(&["push", "-b", "main"]);
    assert!(!out.status.success(), "{}", show(&out));
    assert!(
        repo.rev_parse("refs/heads/jj-hooks-fixup/main").is_none(),
        "temp fixup ref should be cleaned up after import"
    );
    let fixup = repo
        .fixup_commit_for("main")
        .expect("fixup commit should be findable by description");
    assert!(repo.jj_knows_commit(&fixup));
}

// -- hk ---------------------------------------------------------------------

#[test]
fn hk_passing_hooks_pushes() {
    let repo = TestRepo::new();
    repo.write_hk_config(HK_PRE_PUSH_PASSING);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let head = repo.commit_id_of("main");

    let out = repo.jj_hooks(&["push", "-b", "main"]);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(repo.remote_commit("main").as_deref(), Some(head.as_str()));
}

#[test]
fn hk_failing_hooks_abort_push() {
    let repo = TestRepo::new();
    repo.write_hk_config(HK_PRE_PUSH_FAILING);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let remote_before = repo.remote_commit("main");

    let out = repo.jj_hooks(&["push", "-b", "main"]);
    assert!(!out.status.success(), "{}", show(&out));
    assert_eq!(repo.remote_commit("main"), remote_before);
}

#[test]
fn hk_hook_autofix_creates_fixup_ref() {
    let repo = TestRepo::new();
    repo.write_hk_config(HK_PRE_PUSH_AUTOFIX);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let out = repo.jj_hooks(&["push", "-b", "main"]);
    assert!(!out.status.success(), "{}", show(&out));
    assert!(
        repo.rev_parse("refs/heads/jj-hooks-fixup/main").is_none(),
        "temp fixup ref should be cleaned up after import"
    );
    let fixup = repo
        .fixup_commit_for("main")
        .expect("fixup commit should be findable by description");
    assert!(repo.jj_knows_commit(&fixup));
}

// -- runner migration (issue #2) ---------------------------------------------

#[test]
fn runner_autodetect_inside_target_worktree_not_primary() {
    // Regression for issue #2: when the primary workspace has one runner
    // config on disk but the target commit being pushed has a *different*
    // runner config, autodetect must pick the target commit's runner — not
    // the primary's. Repro: primary has `lefthook.yml`, target commit has
    // `hk.pkl` with a failing hook. Pre-fix would autodetect lefthook in
    // primary, run lefthook in the target worktree (which has no
    // `lefthook.yml`), get a clean exit 0 ("no config"), and push the
    // failing hk commit unchecked. Post-fix autodetects hk inside the
    // target worktree, runs the failing config, and aborts the push.
    let repo = TestRepo::new();

    // Build the migration commit on a feature bookmark: write hk.pkl with
    // a hook that always fails, commit, create the bookmark on @-.
    repo.write_hk_config(HK_PRE_PUSH_FAILING);
    let out = repo.jj(&["commit", "-m", "migrate to hk"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "create", "migrate-to-hk", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    // Move the working copy back to main and put `lefthook.yml` (with a
    // passing config so a stale autodetect would happily report success)
    // on disk. Primary now disagrees with the target commit about which
    // runner config exists.
    let out = repo.jj(&["new", "main"]);
    assert!(out.status.success(), "{}", show(&out));
    // After `jj new main`, the working copy is reset to main's tree (no
    // hk.pkl, since main predates the migration commit). Now write the
    // lefthook config so primary has a config that the pre-fix autodetect
    // would latch onto.
    repo.write_lefthook_config(LEFTHOOK_PRE_PUSH_PASSING);

    // Sanity-check the disagreement: primary has lefthook.yml on disk
    // but no hk.pkl; the target commit has hk.pkl (failing) and no
    // lefthook.yml. The pre-fix bug is autodetect picking up the
    // wrong runner here.
    assert!(repo.primary().join("lefthook.yml").exists());
    assert!(!repo.primary().join("hk.pkl").exists());

    let remote_before = repo.remote_commit("migrate-to-hk");

    // No --runner flag, so we exercise the autodetect path that the issue
    // is about. Push must fail because the hk hook fails, not succeed
    // because lefthook silent-skipped on a missing config.
    let out = repo.jj_hooks(&["push", "-b", "migrate-to-hk", "--allow-new"]);
    assert!(
        !out.status.success(),
        "push should abort because hk hook fails:\n{}",
        show(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("No config files with names [\"lefthook\""),
        "lefthook should not be the picked runner (primary's config bled \
         into the target worktree's autodetect):\n{stderr}"
    );
    // Remote did not move.
    assert_eq!(repo.remote_commit("migrate-to-hk"), remote_before);
}

#[test]
fn runner_autodetect_inside_target_worktree_picks_lefthook() {
    // Mirror of the above: target commit has lefthook, primary has hk.
    // Exercises the symmetric scenario so a fix that only handles one
    // direction can't pass.
    let repo = TestRepo::new();

    repo.write_lefthook_config(LEFTHOOK_PRE_PUSH_FAILING);
    let out = repo.jj(&["commit", "-m", "migrate to lefthook"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "create", "migrate-to-lefthook", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let out = repo.jj(&["new", "main"]);
    assert!(out.status.success(), "{}", show(&out));
    repo.write_hk_config(HK_PRE_PUSH_PASSING);

    assert!(repo.primary().join("hk.pkl").exists());
    assert!(!repo.primary().join("lefthook.yml").exists());

    let remote_before = repo.remote_commit("migrate-to-lefthook");

    let out = repo.jj_hooks(&["push", "-b", "migrate-to-lefthook", "--allow-new"]);
    assert!(
        !out.status.success(),
        "push should abort because the target commit's lefthook hook fails:\n{}",
        show(&out)
    );
    assert_eq!(repo.remote_commit("migrate-to-lefthook"), remote_before);
}

#[test]
fn runner_autodetect_skips_when_target_commit_has_no_config() {
    // When the target commit has no hook-runner config at all, the push
    // proceeds with no hooks — even if primary has a config on disk that
    // would have failed. This matches the pre-existing behavior for the
    // no-config case at the workspace level.
    let repo = TestRepo::new();

    // Target commit: no hook configs at all.
    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let head = repo.commit_id_of("main");

    // Primary working copy: a failing lefthook config. Pre-fix would
    // autodetect it and run lefthook against the target worktree (which
    // has no lefthook.yml), get a silent skip, and push. Post-fix
    // autodetects against the target worktree directly, sees no config,
    // and silent-skips by design — same end result, different reasoning.
    repo.write_lefthook_config(LEFTHOOK_PRE_PUSH_FAILING);

    let out = repo.jj_hooks(&["push", "-b", "main"]);
    assert!(
        out.status.success(),
        "target commit has no hook config; push should proceed:\n{}",
        show(&out)
    );
    assert_eq!(repo.remote_commit("main").as_deref(), Some(head.as_str()));
}
