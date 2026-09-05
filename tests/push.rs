//! End-to-end integration tests for the push pipeline.
//!
//! Each test sets up a fresh primary jj+git repo with a local bare remote,
//! writes a pre-commit config, makes a change, and exercises `jj-hooks`
//! against it.

mod harness;

use harness::{
    HK_PRE_PUSH_AUTOFIX, HK_PRE_PUSH_FAILING, HK_PRE_PUSH_PASSING, LEFTHOOK_PRE_PUSH_AUTOFIX,
    LEFTHOOK_PRE_PUSH_FAILING, LEFTHOOK_PRE_PUSH_PASSING, PRE_PUSH_AUTOFIX,
    PRE_PUSH_AUTOFIX_THEN_PASS, PRE_PUSH_FAILING, PRE_PUSH_INDEX_TOUCH_ONLY, PRE_PUSH_PASSING,
    PRE_PUSH_RECORD_RANGE, TestRepo, show,
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
    let out = repo.jj(&["git", "push", "-b", "tmp"]);
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

    let out = repo.jj_hooks(&["--runner", "pre-commit", "push", "-b", "feature"]);
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

// -- first push of a root-parented commit to a fresh remote (#284) -----------
//
// When a brand-new bookmark whose commit's only parent is jj's synthetic
// root commit is pushed to a remote that shares no history, jj-hooks'
// diff-base resolution used to return `{new}^`, an unresolvable git ref.
// hk crashed parsing it; pre-commit mapped it to the null SHA and crashed
// on the resulting invalid revision range. Both are the same single bug,
// so we cover all three backends. The hook config MUST live in the pushed
// commit's tree (autodetect runs in the target worktree), so we write it
// before committing the root-parented commit.

#[test]
fn first_push_root_parented_commit_pre_commit() {
    let repo = TestRepo::new();
    repo.add_empty_remote("fresh");

    // Root-parented commit whose tree carries the hook config.
    let out = repo.jj(&["new", "root()"]);
    assert!(out.status.success(), "{}", show(&out));
    repo.write_pre_commit_config(PRE_PUSH_PASSING);
    let out = repo.jj(&["commit", "-m", "first"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "create", "firstpush", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let head = repo.commit_id_of("firstpush");
    let out = repo.jj_hooks(&[
        "--runner",
        "pre-commit",
        "push",
        "-b",
        "firstpush",
        "--remote",
        "fresh",
    ]);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(head, repo.commit_id_of("firstpush"));
}

#[test]
fn first_push_root_parented_commit_lefthook() {
    let repo = TestRepo::new();
    repo.add_empty_remote("fresh");

    let out = repo.jj(&["new", "root()"]);
    assert!(out.status.success(), "{}", show(&out));
    repo.write_lefthook_config(LEFTHOOK_PRE_PUSH_PASSING);
    let out = repo.jj(&["commit", "-m", "first"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "create", "firstpush", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let head = repo.commit_id_of("firstpush");
    let out = repo.jj_hooks(&["push", "-b", "firstpush", "--remote", "fresh"]);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(head, repo.commit_id_of("firstpush"));
}

#[test]
fn first_push_root_parented_commit_hk() {
    let repo = TestRepo::new();
    repo.add_empty_remote("fresh");

    let out = repo.jj(&["new", "root()"]);
    assert!(out.status.success(), "{}", show(&out));
    repo.write_hk_config(HK_PRE_PUSH_PASSING);
    let out = repo.jj(&["commit", "-m", "first"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "create", "firstpush", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let head = repo.commit_id_of("firstpush");
    let out = repo.jj_hooks(&["push", "-b", "firstpush", "--remote", "fresh"]);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(head, repo.commit_id_of("firstpush"));
}

// A NON-root-parented commit pushed to a fresh/empty remote must still
// resolve to the `{new}^` diff range — NOT all-files. This pins the
// has_real_parent branch of the #284 fix: only a commit whose sole
// parent is jj's synthetic root gets all-files; a normal commit whose
// real parent simply isn't on the fresh remote keeps diffing against
// its parent. A root-detection misfire would silently all-files a
// normal push, so we assert the hook saw a concrete FROM_REF (the
// parent commit) rather than the empty FROM_REF of all-files mode.
#[test]
fn first_push_real_parented_commit_uses_parent_range() {
    let repo = TestRepo::new();
    repo.add_empty_remote("fresh");
    repo.write_pre_commit_config(PRE_PUSH_RECORD_RANGE);

    // A real commit on top of main (its parent is main's tip, a real
    // git commit — not the jj root), on a brand-new bookmark.
    repo.write("feature.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "feature"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "create", "firstreal", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    // The pushed commit itself. pre-commit receives the raw `{new}^`
    // ref string (it does not pre-resolve it), so FROM_REF should be
    // this commit's id with a trailing `^`.
    let new = repo
        .jj(&[
            "log",
            "-r",
            "firstreal",
            "--no-graph",
            "-T",
            "commit_id",
            "--ignore-working-copy",
        ])
        .stdout;
    let new = String::from_utf8_lossy(&new).trim().to_owned();

    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("range");
    let out_path_str = out_path.to_string_lossy().into_owned();

    let out = repo.jj_hooks_with_env(
        &[
            "--runner",
            "pre-commit",
            "push",
            "-b",
            "firstreal",
            "--remote",
            "fresh",
        ],
        &[("JJ_HOOKS_TEST_RANGE_OUT", &out_path_str)],
    );
    assert!(out.status.success(), "{}", show(&out));

    let contents = std::fs::read_to_string(&out_path).unwrap_or_else(|e| {
        panic!(
            "hook didn't write to {}: {e}\n{}",
            out_path.display(),
            show(&out)
        )
    });
    let from_line = contents
        .lines()
        .find_map(|l| l.strip_prefix("FROM="))
        .unwrap_or_else(|| panic!("missing FROM= line in {contents:?}"));
    assert!(
        !from_line.is_empty(),
        "real-parented first push must run in diff-range mode with a \
         concrete FROM_REF, got an empty FROM_REF (all-files mode — the \
         root-detection misfired): {contents:?}\n{}",
        show(&out)
    );
    let from_stem = from_line
        .strip_suffix('^')
        .unwrap_or_else(|| panic!("expected FROM_REF to be `{{new}}^`, got `{from_line}`"));
    assert!(
        new.starts_with(from_stem),
        "expected FROM_REF to be the pushed commit `{new}` with a trailing \
         `^` (`{{new}}^`), got `{from_line}`",
    );
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
    let out = repo.jj_hooks(&["push", "-b", "migrate-to-hk"]);
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

    let out = repo.jj_hooks(&["push", "-b", "migrate-to-lefthook"]);
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

// -- retry-after-fixup (issue #11) ------------------------------------------

#[test]
fn retry_after_fixup_heals_transient_failure_by_default() {
    // Issue #11: when a hook run produces a fixup commit AND
    // reports failure, jj-hooks defaults to re-running the hook
    // backend against the fixup commit. If that re-run is clean,
    // the abort message changes shape: instead of "hook failed +
    // hooks modified files", we get a single "hooks modified
    // files; re-run on fixup commit was clean" line. The push
    // still aborts (the user needs to advance their bookmark to
    // the fixup before re-pushing) but the user can tell at a
    // glance that the failure was transient.
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_AUTOFIX_THEN_PASS);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let remote_before = repo.remote_commit("main");

    let out = repo.jj_hooks(&["--runner", "pre-commit", "push", "-b", "main"]);
    assert!(!out.status.success(), "{}", show(&out));

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("re-run on fixup commit was clean"),
        "expected the retry-healed message, got:\n{stderr}"
    );
    assert!(
        !stderr.contains(": hook failed"),
        "the retry succeeded, so the bare \"hook failed\" line should not appear:\n{stderr}"
    );

    // Push aborted as designed — the user still has to squash the
    // fixup into the bookmark before re-running. Remote unchanged.
    assert_eq!(repo.remote_commit("main"), remote_before);

    // Fixup commit still exists in jj's graph (addressable by hash)
    // so the user can `jj squash` it.
    let fixup = repo
        .fixup_commit_for("main")
        .expect("fixup commit should still be findable after retry");
    assert!(repo.jj_knows_commit(&fixup));
}

#[test]
fn no_retry_after_fixup_flag_restores_pre_0_3_behavior() {
    // The opt-out: `--no-retry-after-fixup` skips the retry pass
    // entirely, so the user sees the original two-line output
    // ("hook failed" + "hooks modified files") with no
    // retry-healed annotation.
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_AUTOFIX_THEN_PASS);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let out = repo.jj_hooks(&[
        "--runner",
        "pre-commit",
        "push",
        "--no-retry-after-fixup",
        "-b",
        "main",
    ]);
    assert!(!out.status.success(), "{}", show(&out));

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(": hook failed"),
        "without retry, the failure line should appear:\n{stderr}"
    );
    assert!(
        stderr.contains("hooks modified files (fixup commit"),
        "without retry, the bare fixup line should appear:\n{stderr}"
    );
    assert!(
        !stderr.contains("re-run on fixup commit was clean"),
        "the retry-healed message must NOT appear when --no-retry-after-fixup is set:\n{stderr}"
    );
}

#[test]
fn retry_after_fixup_still_fails_when_retry_also_fails() {
    // A pure failing hook (no autofix, no fixup commit) goes
    // through the normal failure path — retry-after-fixup never
    // triggers because there's no fixup to re-run against.
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_FAILING);

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let out = repo.jj_hooks(&["--runner", "pre-commit", "push", "-b", "main"]);
    assert!(!out.status.success(), "{}", show(&out));

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(": hook failed"),
        "expected the bare failure line, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("re-run on fixup commit was clean"),
        "no fixup was produced, so no retry should have happened:\n{stderr}"
    );
}

#[test]
fn run_for_revset_uses_full_range_for_multi_commit_revset() {
    // Regression test for the bug where `run_for_revset_outcome`
    // called `jj log -r '<revset>' --limit 1` and treated that
    // single commit as the "to" target — so for a multi-commit
    // revset like `main..tip`, hooks ran against only the tip's
    // 1-commit parent→tip slice. Real-world fallout (sea-621
    // stack): formatting drift introduced by middle commits
    // sailed past the pre-push gate.
    //
    // Fix: the synthesized BookmarkUpdate now uses
    // `roots(<revset>)-` as old_commit and `heads(<revset>)` as
    // new_commit, so hooks see the full diff range.
    //
    // The fixture builds a 3-commit stack on top of main, runs
    // `jj-hp run --stage pre-push 'main..tip-change-id'`, and
    // asserts the hook saw FROM_REF=trunk_tip / TO_REF=stack_tip.
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_RECORD_RANGE);

    // Capture the trunk tip BEFORE we make any new commits — this
    // is what the hook should see as FROM_REF after the fix.
    let trunk_tip = repo
        .jj(&[
            "log",
            "-r",
            "main@origin",
            "--no-graph",
            "-T",
            "commit_id",
            "--ignore-working-copy",
        ])
        .stdout;
    let trunk_tip = String::from_utf8_lossy(&trunk_tip).trim().to_owned();

    // Three-commit stack: A → B → C, all touching different files
    // so the worktree at C reflects work from every layer.
    repo.write("commit_a.txt", "from A\n");
    let out = repo.jj(&["commit", "-m", "commit A"]);
    assert!(out.status.success(), "{}", show(&out));
    repo.write("commit_b.txt", "from B\n");
    let out = repo.jj(&["commit", "-m", "commit B"]);
    assert!(out.status.success(), "{}", show(&out));
    repo.write("commit_c.txt", "from C\n");
    let out = repo.jj(&["commit", "-m", "commit C"]);
    assert!(out.status.success(), "{}", show(&out));

    // Capture the stack tip — what the hook should see as TO_REF.
    // After `jj commit -m C` the working copy is now an empty
    // child commit at @, so the tip of the stack is at @-.
    let stack_tip = repo
        .jj(&[
            "log",
            "-r",
            "@-",
            "--no-graph",
            "-T",
            "commit_id",
            "--ignore-working-copy",
        ])
        .stdout;
    let stack_tip = String::from_utf8_lossy(&stack_tip).trim().to_owned();

    // Output path the hook will write to.
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("range");
    let out_path_str = out_path.to_string_lossy().into_owned();

    let out = repo.jj_hooks_with_env(
        &[
            "--runner",
            "pre-commit",
            "run",
            "--stage",
            "pre-push",
            "main@origin..@-",
        ],
        &[("JJ_HOOKS_TEST_RANGE_OUT", &out_path_str)],
    );
    assert!(out.status.success(), "{}", show(&out));

    let contents = std::fs::read_to_string(&out_path).unwrap_or_else(|e| {
        panic!(
            "hook didn't write to {}: {e}\n{}",
            out_path.display(),
            show(&out)
        )
    });
    // Pre-commit truncates the to-ref to 12 chars in some
    // versions; the from-ref is also passed through. We tolerate
    // a prefix match in either direction.
    let from_line = contents
        .lines()
        .find_map(|l| l.strip_prefix("FROM="))
        .unwrap_or_else(|| panic!("missing FROM= line in {contents:?}"));
    let to_line = contents
        .lines()
        .find_map(|l| l.strip_prefix("TO="))
        .unwrap_or_else(|| panic!("missing TO= line in {contents:?}"));
    assert!(
        trunk_tip.starts_with(from_line) || from_line.starts_with(&trunk_tip),
        "expected FROM_REF to be trunk tip `{trunk_tip}`, got `{from_line}`",
    );
    assert!(
        stack_tip.starts_with(to_line) || to_line.starts_with(&stack_tip),
        "expected TO_REF to be stack tip `{stack_tip}`, got `{to_line}`. \
         If this is the middle commit's SHA, the multi-commit-range fix has regressed.",
    );
}

// -- missing runner binary (issue #17) ---------------------------------------

#[test]
fn missing_runner_binary_surfaces_structured_error() {
    // Regression for issue #17: when the configured runner binary
    // isn't on $PATH, the pre-fix behaviour was a cryptic
    // `jj-hooks: No such file or directory (os error 2)` with no
    // indication of *which* binary couldn't be found. The fix
    // pre-checks $PATH and emits a structured error that names the
    // missing binary and points at the venv/install hint.
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_PASSING);

    // Move main forward so there's actually something to push.
    repo.write("hello.txt", "hello\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    // Sanitized PATH includes only git + jj (and sh, which git's
    // worktree machinery sometimes shells out to). Crucially: no
    // pre-commit, no prek.
    let out = repo.jj_hooks_with_path_allowlist(
        &["--runner", "pre-commit", "push", "-b", "main"],
        &["git", "jj", "sh"],
    );
    assert!(
        !out.status.success(),
        "push should fail when runner binary is missing:\n{}",
        show(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("hook runner `pre-commit` could not be resolved"),
        "stderr should name the missing runner binary, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("No such file or directory (os error 2)"),
        "stderr should not be the cryptic libc message, got:\n{stderr}"
    );
}

#[test]
fn resolver_prek_install_shim_path_wins_over_missing_path_binary() {
    // The issue #17 repro: the user has `prek install`'d a hook that
    // bakes in the venv path (`/…/.venv/bin/prek`), and prek is NOT
    // on $PATH. Pre-fix: jj-hp would try `prek` against the bare
    // PATH, fail to find it, and surface the cryptic os-error-2.
    // Post-fix: layer 2 of the resolver reads `PREK=` out of
    // `.git/hooks/<stage>` and uses that path directly.
    //
    // We simulate "prek installed in venv" by planting a fake prek
    // script *outside* the sandbox PATH (in a dedicated tempdir),
    // writing the install-shim that points at that path, and
    // invoking jj-hp with a PATH that excludes prek entirely.
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_PASSING);

    // Move main forward so there's something to push.
    repo.write("hello.txt", "hello\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    // Simulated "venv" outside any PATH: just a tempdir with a
    // fake prek executable. This one ignores its argv and just
    // exits 0 (it lives outside any env-cleared sandbox, so we
    // can't easily smuggle a marker-file path to it).
    let venv = tempfile::TempDir::new().unwrap();
    let venv_prek = venv.path().join("prek");
    std::fs::write(&venv_prek, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&venv_prek).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&venv_prek, perms).unwrap();
    }

    // Write the `prek install` shim pointing at the venv prek.
    repo.write_prek_shim("pre-push", &venv_prek);

    // Sandboxed PATH: git, jj, sh only — no prek. Pre-fix this would
    // fail with RunnerNotFound; post-fix the resolver finds the
    // venv prek via the shim and runs it.
    let out = repo.jj_hooks_with_path_allowlist_and_extras(
        &[
            "--runner", "prek", "push", "--stage", "pre-push", "-b", "main",
        ],
        &["git", "jj", "sh"],
        &[],
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "push should succeed via shim-resolved prek path:\n{}",
        show(&out)
    );
    assert!(
        !stderr.contains("hook runner `prek` could not be resolved"),
        "RunnerNotFound should not fire — layer 2 should have resolved prek:\n{stderr}"
    );
}

#[test]
fn resolver_pre_commit_install_shim_path_wins_over_missing_path_binary() {
    // Same as the prek shim test above, but exercising pre-commit's
    // shim format. `pre-commit install` writes `INSTALL_PYTHON=<path>`
    // pointing at the venv's Python interpreter and then runs it as
    // `"$INSTALL_PYTHON" -mpre_commit "${ARGS[@]}"`. The resolver
    // therefore needs to splice `[INSTALL_PYTHON, "-mpre_commit"]`
    // (two argv elements) ahead of pre-commit's subcommand args —
    // not just the python path.
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_PASSING);

    repo.write("hello.txt", "hello\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    // Fake "python" that emulates `python -mpre_commit run …`. It
    // only needs to handle the exact argv we send: `-mpre_commit
    // run --hook-stage <stage> --from-ref <from> --to-ref <to>`.
    // We assert that the first arg is `-mpre_commit` (otherwise
    // the resolver forgot to splice it in), then exit 0.
    let venv = tempfile::TempDir::new().unwrap();
    let fake_python = venv.path().join("python");
    let script = r#"#!/bin/sh
if [ "$1" != "-mpre_commit" ]; then
    echo "expected first arg -mpre_commit, got: $@" >&2
    exit 99
fi
exit 0
"#;
    std::fs::write(&fake_python, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&fake_python).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_python, perms).unwrap();
    }

    repo.write_pre_commit_shim("pre-push", &fake_python);

    // Sandboxed PATH: git, jj, sh only — no pre-commit, no python.
    let out = repo.jj_hooks_with_path_allowlist_and_extras(
        &[
            "--runner",
            "pre-commit",
            "push",
            "--stage",
            "pre-push",
            "-b",
            "main",
        ],
        &["git", "jj", "sh"],
        &[],
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "push should succeed via shim-resolved pre-commit (python -mpre_commit):\n{}",
        show(&out)
    );
    assert!(
        !stderr.contains("hook runner `pre-commit` could not be resolved"),
        "RunnerNotFound should not fire — layer 2 should have resolved pre-commit:\n{stderr}"
    );
    // If the resolver dropped the `-mpre_commit` arg, the fake
    // python would have exited 99 and the push would fail with
    // exit code reflecting that — covered by the success assertion
    // above. Belt-and-braces: also check no exit-99 marker.
    assert!(
        !stderr.contains("expected first arg -mpre_commit"),
        "fake python complained — `-mpre_commit` got lost in splicing:\n{stderr}"
    );
}

#[test]
fn resolver_config_runner_bin_wins_over_path() {
    // Layer 1 (jj-hooks.runner-bin.<runner> config) takes precedence
    // over $PATH. Write the override into a JJ_CONFIG file (per-repo
    // config in jj 0.41 lives in `~/.config/jj/repos/<hash>/` keyed
    // to the workspace's opaque id, which env_clear breaks; a
    // JJ_CONFIG-mounted config file sidesteps that and is the
    // documented way to override jj's config in tests).
    //
    // The override points at a fake prek that writes a marker file
    // with "config-prek"; we also plant a *different* prek on the
    // sandbox PATH that would write "path-prek". If layer 1 didn't
    // fire we'd see the PATH one run.
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_PASSING);

    repo.write("hello.txt", "hello\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    // Marker file (parent of repo.tmp so it survives the env_clear
    // sandbox — the fake runner only needs PATH to find sh, and we
    // hard-code the marker path into the script body).
    let log_dir = tempfile::TempDir::new().unwrap();
    let log_path = log_dir.path().join("ran.txt");
    let log_str = log_path.to_string_lossy().into_owned();

    // The "config-resolved" prek, planted outside any PATH.
    let venv = tempfile::TempDir::new().unwrap();
    let config_prek = venv.path().join("prek");
    let config_script = format!("#!/bin/sh\necho \"config-prek: $@\" >> \"{log_str}\"\nexit 0\n");
    std::fs::write(&config_prek, config_script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&config_prek).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&config_prek, perms).unwrap();
    }

    // JJ_CONFIG file with the runner-bin override + identity (the
    // env_clear sandbox loses user-level identity, so jj would
    // refuse to push without this).
    let jj_config = repo.tmp.path().join("jj-config.toml");
    let jj_config_body = format!(
        r#"[user]
name = "jj-hooks tests"
email = "tests@jj-hooks.invalid"

[jj-hooks.runner-bin]
prek = "{}"
"#,
        config_prek.display()
    );
    std::fs::write(&jj_config, jj_config_body).unwrap();

    // Build the sandbox bin dir manually so we can plant both the
    // git/jj/sh symlinks AND a bait `prek` that records a different
    // label. (The harness helper doesn't let us mix both in one
    // step plus customize child env beyond the defaults.)
    let bin_dir = repo.sandbox_bin_dir();
    let _ = std::fs::remove_dir_all(&bin_dir);
    std::fs::create_dir(&bin_dir).unwrap();
    let parent_path = std::env::var_os("PATH").unwrap();
    for name in ["git", "jj", "sh"] {
        let src = std::env::split_paths(&parent_path)
            .find_map(|d| {
                let c = d.join(name);
                c.is_file().then_some(c)
            })
            .unwrap_or_else(|| panic!("{name} not on parent PATH"));
        #[cfg(unix)]
        std::os::unix::fs::symlink(&src, bin_dir.join(name)).unwrap();
    }
    let bait_prek = bin_dir.join("prek");
    let bait_script = format!("#!/bin/sh\necho \"path-prek: $@\" >> \"{log_str}\"\nexit 0\n");
    std::fs::write(&bait_prek, bait_script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&bait_prek).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bait_prek, perms).unwrap();
    }

    let jj_hooks_bin = env!("CARGO_BIN_EXE_jj-hooks");
    let out = std::process::Command::new(jj_hooks_bin)
        .args([
            "--runner", "prek", "push", "--stage", "pre-push", "-b", "main",
        ])
        .current_dir(repo.primary())
        .env_clear()
        .env("PATH", &bin_dir)
        .env("JJ_CONFIG", &jj_config)
        .env("JJ_HOOKS_LOG", "info")
        .env("HOME", repo.tmp.path())
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "push should succeed via config-resolved prek:\n{}",
        show(&out)
    );
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        log.contains("config-prek"),
        "config-resolved prek should have run, log was: {log:?}\n{}",
        show(&out)
    );
    assert!(
        !log.contains("path-prek"),
        "PATH-resolved prek must NOT have run when config-runner-bin is set: {log:?}\n{}",
        show(&out)
    );
}
