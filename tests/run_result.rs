mod harness;

use harness::{TestRepo, show};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::process::Output;

fn fixture(script: &str) -> (TestRepo, String, String) {
    let repo = TestRepo::new();
    let from = repo.commit_id_of("main");
    repo.write("hk.pkl", "fixture\n");
    repo.write("change.txt", "changed\n");
    let out = repo.jj(&["commit", "-m", "change"]);
    assert!(out.status.success(), "{}", show(&out));
    let to = repo.commit_id_of("@-");
    let runner = repo.tmp.path().join("hk-runner");
    std::fs::write(&runner, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).unwrap();
    let value = format!("\"{}\"", runner.display());
    let out = repo.jj(&["config", "set", "--repo", "jj-hooks.runner-bin.hk", &value]);
    assert!(out.status.success(), "{}", show(&out));
    (repo, from, to)
}

fn run(
    repo: &TestRepo,
    from: &str,
    to: &str,
    extra: &[&str],
    env: &[(&str, &str)],
) -> (Output, Value) {
    let path = repo.tmp.path().join("result.json");
    let mut args = vec![
        "run",
        "--stage",
        "pre-push",
        "--from",
        from,
        "--to",
        to,
        "--result-json",
        path.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    let out = repo.jj_hooks_with_env(&args, env);
    let result: Value =
        serde_json::from_slice(&std::fs::read(path).expect("result exists")).unwrap();
    (out, result)
}

#[test]
fn runner_completed_reports_exit_and_endpoints() {
    let (repo, from, to) = fixture("exit 7");
    let (out, result) = run(&repo, &from, &to, &[], &[]);
    assert!(!out.status.success(), "{}", show(&out));
    assert_eq!(result["phase"], "runner-completed");
    assert_eq!(result["exit"], 7);
    assert_eq!(result["from"], from);
    assert_eq!(result["to"], to);
    assert_eq!(result["config"], "hk.pkl");
}

#[test]
fn no_config_is_success() {
    let repo = TestRepo::new();
    let from = repo.commit_id_of("main");
    repo.write("x", "x");
    assert!(repo.jj(&["commit", "-m", "x"]).status.success());
    let to = repo.commit_id_of("@-");
    let (out, result) = run(&repo, &from, &to, &[], &[]);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(result["phase"], "no-config");
    assert_eq!(result["exit"], 0);
}

#[test]
fn failed_setup_is_reported() {
    let (repo, from, to) = fixture("exit 0");
    let out = repo.jj(&[
        "config",
        "set",
        "--repo",
        "jj-hooks.setup",
        r#"[{ name = "broken-setup", run = ["false"] }]"#,
    ]);
    assert!(out.status.success(), "{}", show(&out));
    let (out, result) = run(&repo, &from, &to, &[], &[]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("broken-setup"),
        "{}",
        show(&out)
    );
    assert_eq!(result["phase"], "setup-failed");
}

#[test]
fn fixup_result_survives_retry() {
    let (repo, from, to) = fixture("echo fixed > change.txt\nexit 1");
    let (out, result) = run(&repo, &from, &to, &[], &[]);
    assert!(!out.status.success());
    assert_eq!(result["phase"], "fixup");
    assert_eq!(result["from"], from);
    assert_eq!(result["to"], to);
    assert_eq!(result["fixup"].as_str().unwrap().len(), 40);
}

#[test]
fn runner_inherits_gate_environment() {
    let (repo, from, to) = fixture(
        "test \"$RJC_MOON_TARGETS:$RJC_MOON_REPORT:$RJC_GATE_REPORT:$MOON_BASE:$MOON_HEAD\" = 'a:b:c:d:e'",
    );
    let env = [
        ("RJC_MOON_TARGETS", "a"),
        ("RJC_MOON_REPORT", "b"),
        ("RJC_GATE_REPORT", "c"),
        ("MOON_BASE", "d"),
        ("MOON_HEAD", "e"),
    ];
    let (out, result) = run(&repo, &from, &to, &[], &env);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(result["phase"], "runner-completed");
    assert_eq!(result["exit"], 0);
}

#[test]
fn worktree_failure_still_writes_setup_result() {
    let (repo, from, _) = fixture("exit 0");
    let (out, result) = run(
        &repo,
        &from,
        "0123456789012345678901234567890123456789",
        &[],
        &[],
    );
    assert!(!out.status.success());
    assert_eq!(result["phase"], "setup-failed");
}
