//! End-to-end integration tests for the push pipeline.
//!
//! Each test sets up a fresh primary jj+git repo with a local bare remote,
//! writes a pre-commit config, makes a change, and exercises `jj-hooks`
//! against it.

mod harness;

use harness::{
    HK_PRE_PUSH_AUTOFIX, HK_PRE_PUSH_FAILING, HK_PRE_PUSH_PASSING, LEFTHOOK_PRE_PUSH_AUTOFIX,
    LEFTHOOK_PRE_PUSH_FAILING, LEFTHOOK_PRE_PUSH_PASSING, PRE_PUSH_AUTOFIX, PRE_PUSH_FAILING,
    PRE_PUSH_PASSING, TestRepo, show,
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
