//! Integration tests for `jj-hp run --all-files`.
//!
//! Without `--all-files`, hooks run against the diff range
//! `roots(<revset>)- .. heads(<revset>)`, so a hook gated to files
//! the diff doesn't touch is silent-skipped. The flag tells each
//! runner to ignore the file selection and run every hook against
//! every tracked file — the standard "lint everything now" mode.
//!
//! Per-runner mapping (verified against each tool's --help):
//!   pre-commit / prek: --all-files (replaces --from-ref/--to-ref)
//!   hk:                -a / --all  (replaces --from-ref/--to-ref)
//!   lefthook:          --all-files (replaces per-file selection)

mod harness;

use harness::{TestRepo, show};

/// pre-commit hook gated by `files:` regex so it only runs when
/// `target.txt` is in the file list. Always fails when it runs —
/// the BDD assertion is "did it run?", measured by exit code.
const PRE_PUSH_GATED_TO_TARGET_FILE: &str = r#"
repos:
  - repo: local
    hooks:
      - id: gated
        name: gated
        entry: 'false'
        language: system
        files: '^target\.txt$'
        stages: [pre-push]
        pass_filenames: false
"#;

/// BDD: a hook gated to `target.txt` is silent-skipped on a push
/// that doesn't touch it. This is the precondition the all-files
/// test relies on — without it, the all-files test could pass for
/// the wrong reason (hook always ran, flag did nothing).
#[test]
fn glob_gated_hook_skipped_when_diff_does_not_touch_target() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_GATED_TO_TARGET_FILE);

    // Commit target.txt at parent so it's in the tree but NOT in
    // the diff range; then move forward via an unrelated file.
    repo.write("target.txt", "static\n");
    let out = repo.jj(&["commit", "-m", "add target"]);
    assert!(out.status.success(), "{}", show(&out));
    repo.write("unrelated.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "unrelated edit"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let out = repo.jj_hooks(&["--runner", "pre-commit", "run", "--stage", "pre-push", "@-"]);
    assert!(
        out.status.success(),
        "the gated hook should silent-skip (target.txt not in the diff):\n{}",
        show(&out)
    );
}

/// BDD: with `--all-files`, the same gated hook fires because
/// target.txt IS in the working tree, even though the diff range
/// doesn't include it.
#[test]
fn all_files_flag_runs_glob_gated_hook_against_full_tree() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_GATED_TO_TARGET_FILE);

    repo.write("target.txt", "static\n");
    let out = repo.jj(&["commit", "-m", "add target"]);
    assert!(out.status.success(), "{}", show(&out));
    repo.write("unrelated.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "unrelated edit"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let out = repo.jj_hooks(&[
        "--runner",
        "pre-commit",
        "run",
        "--stage",
        "pre-push",
        "--all-files",
        "@-",
    ]);
    assert!(
        !out.status.success(),
        "--all-files should have made the gated hook fire against \
         target.txt (which is in the tree but not the diff); hook \
         exits non-zero so jj-hp should too:\n{}",
        show(&out)
    );
}

/// BDD: the same flag works through the prek backend (drop-in
/// replacement for pre-commit). Mirrors the pre-commit test so a
/// runner-specific argv bug can't pass one and fail the other.
#[test]
fn all_files_flag_works_with_prek() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_GATED_TO_TARGET_FILE);

    repo.write("target.txt", "static\n");
    let out = repo.jj(&["commit", "-m", "add target"]);
    assert!(out.status.success(), "{}", show(&out));
    repo.write("unrelated.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "unrelated edit"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let out = repo.jj_hooks(&[
        "--runner",
        "prek",
        "run",
        "--stage",
        "pre-push",
        "--all-files",
        "@-",
    ]);
    assert!(
        !out.status.success(),
        "prek --all-files should fire the gated hook:\n{}",
        show(&out)
    );
}

/// hk's gated step uses a `glob` field — same idea, different
/// runner.
const HK_PRE_PUSH_GATED_TO_TARGET_FILE: &str = r#"amends "package://github.com/jdx/hk/releases/download/v1.45.0/hk@1.45.0#/Config.pkl"

local linters = new Mapping<String, Step> {
    ["gated"] {
        glob = "target.txt"
        check = "false"
    }
}

hooks {
    ["pre-push"] {
        fix = false
        steps = linters
    }
}
"#;

#[test]
fn all_files_flag_works_with_hk() {
    let repo = TestRepo::new();
    repo.write_hk_config(HK_PRE_PUSH_GATED_TO_TARGET_FILE);

    repo.write("target.txt", "static\n");
    let out = repo.jj(&["commit", "-m", "add target"]);
    assert!(out.status.success(), "{}", show(&out));
    repo.write("unrelated.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "unrelated edit"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let out = repo.jj_hooks(&[
        "--runner",
        "hk",
        "run",
        "--stage",
        "pre-push",
        "--all-files",
        "@-",
    ]);
    assert!(
        !out.status.success(),
        "hk --all-files should fire the gated step against target.txt:\n{}",
        show(&out)
    );
}

/// lefthook gates per-command with `glob` too. Symmetric coverage.
const LEFTHOOK_PRE_PUSH_GATED_TO_TARGET_FILE: &str = r#"pre-push:
  commands:
    gated:
      glob: "target.txt"
      run: "false"
"#;

#[test]
fn all_files_flag_works_with_lefthook() {
    let repo = TestRepo::new();
    repo.write_lefthook_config(LEFTHOOK_PRE_PUSH_GATED_TO_TARGET_FILE);

    repo.write("target.txt", "static\n");
    let out = repo.jj(&["commit", "-m", "add target"]);
    assert!(out.status.success(), "{}", show(&out));
    repo.write("unrelated.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "unrelated edit"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let out = repo.jj_hooks(&[
        "--runner",
        "lefthook",
        "run",
        "--stage",
        "pre-push",
        "--all-files",
        "@-",
    ]);
    assert!(
        !out.status.success(),
        "lefthook --all-files should fire the gated command against \
         target.txt:\n{}",
        show(&out)
    );
}
