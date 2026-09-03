//! Integration tests for the gate-cache mechanism (Mode A / T1): the gate
//! subprocess's `CARGO_TARGET_DIR` is pointed at the PRIMARY workspace's
//! `target/` so the gate reuses the user's warm dev builds instead of paying a
//! cold build in the ephemeral `/tmp` worktree.
//!
//! These drive the real CLI push pipeline (`jj-hp push`) and the batch API,
//! asserting what `CARGO_TARGET_DIR` the hook child actually observed via the
//! `PRE_PUSH_RECORD_CARGO_TARGET_DIR` fixture. See
//! `docs/designs/tools/jj-hp-gate-worktree-cost.md`.

mod harness;

use harness::{PRE_PUSH_RECORD_CARGO_TARGET_DIR, TestRepo, show};

/// The primary workspace's canonical `target/` — the value the gate should
/// inject. `jj-hp` canonicalizes `workspace_root` via dunce, which on unix
/// equals `std::fs::canonicalize`.
fn expected_target_dir(repo: &TestRepo) -> String {
    std::fs::canonicalize(repo.primary())
        .unwrap()
        .join("target")
        .to_string_lossy()
        .into_owned()
}

/// Run a passing `jj-hp push` of `main` with the CTD-recording hook, returning
/// what `CARGO_TARGET_DIR` the hook child saw. `extra_env` is spliced into
/// jj-hp's own environment (so a decoy `CARGO_TARGET_DIR` or the opt-out can be
/// injected).
fn push_and_read_ctd(repo: &TestRepo, extra_env: &[(&str, &str)]) -> String {
    let out_dir = tempfile::tempdir().unwrap();
    let out_path = out_dir.path().join("ctd");
    let out_path_str = out_path.to_string_lossy().into_owned();

    let mut env: Vec<(&str, &str)> = vec![("JJ_HOOKS_TEST_CTD_OUT", &out_path_str)];
    env.extend_from_slice(extra_env);

    let out = repo.jj_hooks_with_env(&["--runner", "pre-commit", "push", "-b", "main"], &env);
    assert!(out.status.success(), "{}", show(&out));

    std::fs::read_to_string(&out_path).unwrap_or_else(|e| {
        panic!(
            "hook didn't write to {}: {e}\n{}",
            out_path.display(),
            show(&out)
        )
    })
}

/// Advance `main` one commit so there's something to push.
fn advance_main(repo: &TestRepo, rel: &str) {
    repo.write(rel, "x\n");
    let out = repo.jj(&["commit", "-m", "advance"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));
}

/// The gate points `CARGO_TARGET_DIR` at the primary `target/`.
///
/// RED before T1: with no injection the hook child sees `unset` (the ephemeral
/// worktree inherits no `CARGO_TARGET_DIR`), so this assertion of
/// `<primary>/target` fails. GREEN after: the gate injects it.
#[test]
fn gate_points_cargo_target_dir_at_primary() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_RECORD_CARGO_TARGET_DIR);
    advance_main(&repo, "new.txt");

    let seen = push_and_read_ctd(&repo, &[]);
    assert_eq!(
        seen,
        expected_target_dir(&repo),
        "the gate subprocess must see CARGO_TARGET_DIR = <primary>/target"
    );
}

/// jj-hp's injected value beats a decoy `CARGO_TARGET_DIR` inherited from the
/// parent env — the injection is applied AFTER `apply_repo_env` and wins.
#[test]
fn gate_value_beats_inherited_decoy() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_RECORD_CARGO_TARGET_DIR);
    advance_main(&repo, "new.txt");

    let seen = push_and_read_ctd(&repo, &[("CARGO_TARGET_DIR", "/decoy/target")]);
    assert_eq!(
        seen,
        expected_target_dir(&repo),
        "jj-hp's CARGO_TARGET_DIR must win over an inherited decoy value"
    );
}

/// With the opt-out set, the gate does NOT inject — the child sees the decoy
/// inherited value byte-for-byte (never cleared).
#[test]
fn opt_out_is_byte_identical_fallback() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_RECORD_CARGO_TARGET_DIR);
    advance_main(&repo, "new.txt");

    let seen = push_and_read_ctd(
        &repo,
        &[
            ("CARGO_TARGET_DIR", "/decoy/target"),
            ("JJ_HOOKS_NO_GATE_CACHE", "1"),
        ],
    );
    assert_eq!(
        seen, "/decoy/target",
        "the opt-out must leave the inherited CARGO_TARGET_DIR untouched"
    );
}

/// The jj-config opt-out (`jj-hooks.gate-cache = off`) disables injection too.
#[test]
fn config_opt_out_disables_injection() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_RECORD_CARGO_TARGET_DIR);
    let out = repo.jj(&["config", "set", "--repo", "jj-hooks.gate-cache", "off"]);
    assert!(out.status.success(), "{}", show(&out));
    advance_main(&repo, "new.txt");

    let seen = push_and_read_ctd(&repo, &[("CARGO_TARGET_DIR", "/decoy/target")]);
    assert_eq!(
        seen, "/decoy/target",
        "jj config `jj-hooks.gate-cache = off` must disable injection"
    );
}

/// Frozen-contract guard: with gate-cache enabled, the child still has NO
/// git-family repo-location var (composes with the existing strip), and no
/// stray extra var appears — only `CARGO_TARGET_DIR` is added.
#[test]
fn injection_does_not_reintroduce_git_family_leak() {
    let repo = TestRepo::new();
    // A hook that reports BOTH the injected CTD and whether GIT_DIR leaked.
    let fixture = r#"
repos:
  - repo: local
    hooks:
      - id: record-ctd-and-git
        name: record-ctd-and-git
        entry: sh -c 'printf "CTD=%s GD=%s" "${CARGO_TARGET_DIR:-unset}" "${GIT_DIR:-unset}" > "$JJ_HOOKS_TEST_CTD_OUT"'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;
    repo.write_pre_commit_config(fixture);
    advance_main(&repo, "new.txt");
    // NOTE: no decoy GIT_DIR is injected into jj-hp's own env here — jj-hp
    // itself shells out to git for its bookkeeping, so a bogus GIT_DIR would
    // corrupt jj-hp, not just the child (the inheritance-strip path is unit-
    // tested in `repo_env`). This asserts the everyday case: the child sees
    // the injected CTD and NO git-family var, so the injection did not
    // reintroduce the leak the strip guards against.
    let seen = push_and_read_ctd(&repo, &[]);
    assert_eq!(
        seen,
        format!("CTD={} GD=unset", expected_target_dir(&repo)),
        "gate must inject CARGO_TARGET_DIR AND keep the git-family strip intact"
    );
}

/// The setup-step seam twins the hook seam: a `jj-hooks.setup` step run inside
/// the worktree also sees the injected `CARGO_TARGET_DIR = <primary>/target`.
#[test]
fn setup_step_also_sees_injected_cargo_target_dir() {
    let repo = TestRepo::new();
    // A hook that just passes; the assertion is on the setup step's output.
    repo.write_pre_commit_config(PRE_PUSH_RECORD_CARGO_TARGET_DIR);

    // The setup step records CTD into a file under the primary (absolute path
    // via $JJ_HOOKS_WORKSPACE, which the step env carries). gitignore it so it
    // doesn't become fixup-worthy content.
    repo.write(".gitignore", "setup_ctd\nnew.txt\n");
    let out = repo.jj(&[
        "config",
        "set",
        "--repo",
        "jj-hooks.setup",
        r#"[{ run = ["sh", "-c", "printf %s \"${CARGO_TARGET_DIR:-unset}\" > \"$JJ_HOOKS_WORKSPACE/setup_ctd\""] }]"#,
    ]);
    assert!(out.status.success(), "{}", show(&out));

    advance_main(&repo, "new.txt");
    let _ = push_and_read_ctd(&repo, &[]);

    let seen = std::fs::read_to_string(repo.primary().join("setup_ctd")).unwrap();
    assert_eq!(
        seen,
        expected_target_dir(&repo),
        "the setup step must also see CARGO_TARGET_DIR = <primary>/target"
    );
}
