//! Integration tests for `jj-hooks.setup` — repo-level config that
//! declares commands to run inside the ephemeral worktree before
//! hooks fire.
//!
//! The motivating use case (issue #9) is `node_modules`: hooks like
//! `tsc` need project-local install resources that aren't in the
//! committed tree, so the ephemeral worktree starts without them and
//! the hook fails with `command not found`. A user-declared setup
//! command (`bun install`, `pnpm install --frozen-lockfile`, etc.)
//! restores them before each hook run.

mod harness;

use harness::{TestRepo, show};

/// A hook that fails unless a marker file `setup_ran` exists in the
/// worktree. Mirrors the issue #9 failure shape: the hook only
/// succeeds if some prior step (a `bun install`-style setup command)
/// has put resources in place.
const PRE_PUSH_REQUIRES_MARKER: &str = r#"
repos:
  - repo: local
    hooks:
      - id: needs-setup
        name: needs-setup
        entry: sh -c '[ -e setup_ran ] || { echo "setup_ran marker is missing" >&2; exit 1; }'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;

/// A hook that records its own execution into a marker file. Used to
/// detect whether the hook *ran at all* — we expect this to be false
/// when a preceding setup step failed and aborted the pipeline.
const PRE_PUSH_TOUCHES_MARKER: &str = r#"
repos:
  - repo: local
    hooks:
      - id: marker
        name: marker
        entry: sh -c 'touch hook_ran'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;

/// BDD: with `jj-hooks.setup` declaring a single step that creates
/// the `setup_ran` marker, the hook (which requires the marker)
/// passes and the push succeeds. Without the setup step the hook
/// would fail with the same "marker missing" error the issue
/// reports for `tsc: command not found`.
#[test]
fn setup_step_runs_in_worktree_before_hook() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_REQUIRES_MARKER);

    // gitignore the marker the setup step creates. This mirrors the
    // issue #9 use case: the setup step's outputs (`node_modules/`,
    // `.venv/`, etc.) are gitignored — that's precisely why they're
    // absent from the ephemeral worktree's initial checkout. A
    // user whose setup step writes non-gitignored files would be
    // mixing install resources with committed content, which is a
    // separate (legitimately fixup-worthy) condition.
    repo.write(".gitignore", "setup_ran\n");

    // Declare a single setup step that creates the marker. The
    // worktree starts without `setup_ran` (it's gitignored AND
    // never committed); the setup step has to be what creates it.
    let out = repo.jj(&[
        "config",
        "set",
        "--repo",
        "jj-hooks.setup",
        r#"[{ run = ["sh", "-c", "touch setup_ran"] }]"#,
    ]);
    assert!(out.status.success(), "{}", show(&out));

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let head = repo.commit_id_of("main");

    let out = repo.jj_hooks(&["--runner", "pre-commit", "push", "-b", "main"]);
    assert!(
        out.status.success(),
        "the setup step should have created `setup_ran` before the hook \
         ran; without setup the hook fails with `marker is missing`:\n{}",
        show(&out)
    );
    assert_eq!(repo.remote_commit("main").as_deref(), Some(head.as_str()));
}

/// BDD: when a setup step exits non-zero, the push aborts and the
/// hook never runs. We assert "hook didn't run" indirectly via the
/// abort exit code + the setup step's name appearing in stderr.
#[test]
fn setup_step_failure_aborts_before_hook_runs() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(PRE_PUSH_TOUCHES_MARKER);

    // Setup step that always fails. The push must abort here, before
    // pre-commit is ever invoked.
    let out = repo.jj(&[
        "config",
        "set",
        "--repo",
        "jj-hooks.setup",
        r#"[{ name = "broken-setup", run = ["false"] }]"#,
    ]);
    assert!(out.status.success(), "{}", show(&out));

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let remote_before = repo.remote_commit("main");

    let out = repo.jj_hooks(&["--runner", "pre-commit", "push", "-b", "main"]);
    assert!(
        !out.status.success(),
        "push must abort when a setup step fails:\n{}",
        show(&out)
    );

    // Failure message should name the step so the user can find it
    // in their config. We accept the named form ("broken-setup") or
    // the bare "setup" keyword — either is informative.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("broken-setup"),
        "abort message should reference the failed step `broken-setup`:\n{stderr}"
    );

    // Remote did not move.
    assert_eq!(repo.remote_commit("main"), remote_before);
}

/// BDD: multiple setup steps run in declared order. Steps 1 and 2
/// each append a token to `order`; the hook reads `order` and
/// asserts the line ordering is `step1` then `step2`.
#[test]
fn multiple_setup_steps_run_in_declared_order() {
    let repo = TestRepo::new();

    // Hook reads `order` and fails unless its content is exactly
    // "step1\nstep2\n". We compare against a freshly-written
    // expected file so the inline yaml doesn't have to embed a
    // literal newline (which pre-commit's yaml parser dislikes).
    let hook_config = r#"
repos:
  - repo: local
    hooks:
      - id: check-order
        name: check-order
        entry: sh -c 'printf "step1\nstep2\n" > expected && diff -u expected order'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;
    repo.write_pre_commit_config(hook_config);

    // gitignore `order` and `expected` — they're install-time
    // outputs of the setup steps, same shape as `node_modules/` in
    // the issue #9 use case.
    repo.write(".gitignore", "order\nexpected\n");

    let out = repo.jj(&[
        "config",
        "set",
        "--repo",
        "jj-hooks.setup",
        r#"[
          { run = ["sh", "-c", "echo step1 >> order"] },
          { run = ["sh", "-c", "echo step2 >> order"] },
        ]"#,
    ]);
    assert!(out.status.success(), "{}", show(&out));

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let head = repo.commit_id_of("main");

    let out = repo.jj_hooks(&["--runner", "pre-commit", "push", "-b", "main"]);
    assert!(
        out.status.success(),
        "both steps should run in declared order so `order` reads `step1\\nstep2`:\n{}",
        show(&out)
    );
    assert_eq!(repo.remote_commit("main").as_deref(), Some(head.as_str()));
}

/// BDD: setup + hook subprocesses see `JJ_HOOKS_WORKSPACE`
/// pointing at the workspace `jj-hp` was invoked from. The setup
/// step uses it to copy a gitignored resource from the invocation
/// workspace into the ephemeral worktree (mirroring the
/// `node_modules` use case from issue #9).
#[test]
fn setup_step_sees_workspace_env() {
    let repo = TestRepo::new();

    // gitignore the resource so jj doesn't snapshot it into the
    // target commit's tree — without this, the file would land in
    // the worktree directly and the env var path would be untested.
    repo.write(".gitignore", "shared_resource\n");

    // Hook fails unless the resource is present in the worktree
    // *with content the setup step copied from primary*.
    let hook_config = r#"
repos:
  - repo: local
    hooks:
      - id: check-shared
        name: check-shared
        entry: sh -c 'grep -q "from primary" shared_resource || { echo "shared_resource missing or wrong content" >&2; exit 1; }'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;
    repo.write_pre_commit_config(hook_config);

    // Setup step copies the file in using $JJ_HOOKS_PRIMARY_WORKSPACE.
    let out = repo.jj(&[
        "config",
        "set",
        "--repo",
        "jj-hooks.setup",
        r#"[{ run = ["sh", "-c", "cp \"$JJ_HOOKS_WORKSPACE/shared_resource\" ."] }]"#,
    ]);
    assert!(out.status.success(), "{}", show(&out));

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    // Create the gitignored shared resource AFTER the commit so it
    // stays uncommitted in primary's working copy. This is the
    // shape `node_modules` has: present in primary, absent from the
    // ephemeral worktree's checkout.
    std::fs::write(repo.primary().join("shared_resource"), "from primary\n").unwrap();

    let head = repo.commit_id_of("main");

    let out = repo.jj_hooks(&["--runner", "pre-commit", "push", "-b", "main"]);
    assert!(
        out.status.success(),
        "the setup step should resolve $JJ_HOOKS_WORKSPACE to the invocation \
         workspace and copy `shared_resource` into the ephemeral worktree:\n{}",
        show(&out)
    );
    assert_eq!(repo.remote_commit("main").as_deref(), Some(head.as_str()));
}

/// BDD: when no `jj-hooks.setup` is configured, the push pipeline
/// behaves exactly as before — no surprise no-ops, no spurious
/// "running setup step" log lines, no extra failure modes.
#[test]
fn no_setup_config_is_a_silent_no_op() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(harness::PRE_PUSH_PASSING);

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

/// BDD: a chatty-but-successful setup step (e.g. `bun install`
/// dumping its "+ pkg@x" banner) must not leak its output to the
/// user's terminal. The user-visible noise was the impetus for the
/// always-capture change; this test pins it.
#[test]
fn successful_setup_step_output_is_suppressed() {
    let repo = TestRepo::new();
    repo.write_pre_commit_config(harness::PRE_PUSH_PASSING);

    // We need to distinguish "subprocess wrote NOISE to its stdout"
    // from "we logged the argv that includes NOISE". jj-hooks emits a
    // tracing INFO line containing the argv when it spawns a setup
    // step (see `setup::run_steps`), so if NOISE appears verbatim in
    // the argv it shows up in the captured INFO stream and the test
    // can't tell whether the subprocess actually leaked.
    //
    // Solution: have the subprocess assemble the marker from two
    // halves so the argv contains the halves but neither contains
    // the joined marker. The marker only appears in the
    // subprocess's stdout/stderr — exactly what we want to assert
    // absence of.
    const NOISE_LEFT: &str = "==CHATTY_BANNER_LEFT_HALF";
    const NOISE_RIGHT: &str = "_AND_RIGHT_HALF_DO_NOT_LEAK==";
    const NOISE_JOINED: &str = "==CHATTY_BANNER_LEFT_HALF_AND_RIGHT_HALF_DO_NOT_LEAK==";
    const STDERR_LEFT: &str = "==STDERR_BANNER_";
    const STDERR_RIGHT: &str = "PART_DO_NOT_LEAK==";
    const STDERR_JOINED: &str = "==STDERR_BANNER_PART_DO_NOT_LEAK==";

    let setup_cmd = format!(
        r#"printf '%s%s\n' '{NOISE_LEFT}' '{NOISE_RIGHT}'; printf '%s%s\n' '{STDERR_LEFT}' '{STDERR_RIGHT}' >&2"#
    );
    let out = repo.jj(&[
        "config",
        "set",
        "--repo",
        "jj-hooks.setup",
        &format!(r#"[{{ name = "loud", run = ["sh", "-c", "{setup_cmd}"] }}]"#),
    ]);
    assert!(out.status.success(), "{}", show(&out));

    repo.write("new.txt", "x\n");
    let out = repo.jj(&["commit", "-m", "second"]);
    assert!(out.status.success(), "{}", show(&out));
    let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let out = repo.jj_hooks(&["--runner", "pre-commit", "push", "-b", "main"]);
    assert!(
        out.status.success(),
        "push should succeed with a passing hook:\n{}",
        show(&out)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains(NOISE_JOINED) && !stderr.contains(NOISE_JOINED),
        "setup step stdout banner leaked to user terminal:\nstdout: {stdout}\nstderr: {stderr}",
    );
    assert!(
        !stdout.contains(STDERR_JOINED) && !stderr.contains(STDERR_JOINED),
        "setup step stderr banner leaked to user terminal:\nstdout: {stdout}\nstderr: {stderr}",
    );
}
