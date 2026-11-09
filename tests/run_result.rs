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

fn assert_fixup(repo: &TestRepo, result: &Value) {
    let id = result["fixup"].as_str().unwrap();
    assert_eq!(id.len(), 40);
    let out = std::process::Command::new("git")
        .current_dir(repo.primary())
        .args(["show", &format!("{id}:change.txt")])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(out.stdout, b"fixed\n");
    let out = repo.jj(&["log", "--no-graph", "-r", id, "-T", "commit_id"]);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), id);
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
    assert!(result["config"].is_null());
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
    assert_eq!(result["exit"], 0);
}

#[test]
fn fixup_result_survives_retry() {
    let (repo, from, to) = fixture("echo fixed > change.txt\nexit 1");
    let (out, result) = run(&repo, &from, &to, &[], &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains(result["fixup"].as_str().unwrap()));
    assert_eq!(result["phase"], "fixup");
    assert_eq!(result["exit"], 0);
    assert_eq!(result["from"], from);
    assert_eq!(result["to"], to);
    assert_fixup(&repo, &result);
}

#[test]
fn runner_inherits_gate_environment() {
    let (repo, from, to) = fixture(
        "test \"$RJC_MOON_TARGETS:$RJC_MOON_REPORT:$RJC_GATE_REPORT:$MOON_BASE:$MOON_HEAD\" = 'a:b:c:d:e'",
    );
    repo.write(".envrc", "fixture\n");
    let bin = repo.tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let direnv = bin.join("direnv");
    std::fs::write(&direnv, "#!/bin/sh\nprintf '%s\\n' '{\"RJC_MOON_TARGETS\":\"wrong\",\"RJC_MOON_REPORT\":\"wrong\",\"RJC_GATE_REPORT\":\"wrong\",\"MOON_BASE\":\"wrong\",\"MOON_HEAD\":\"wrong\"}'\n").unwrap();
    std::fs::set_permissions(&direnv, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let env = [
        ("RJC_MOON_TARGETS", "a"),
        ("RJC_MOON_REPORT", "b"),
        ("RJC_GATE_REPORT", "c"),
        ("MOON_BASE", "d"),
        ("MOON_HEAD", "e"),
        ("PATH", &path),
    ];
    let (out, result) = run(&repo, &from, &to, &[], &env);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(result["phase"], "runner-completed");
    assert_eq!(result["exit"], 0);
}

#[test]
fn worktree_failure_still_writes_tool_result() {
    let (repo, from, _) = fixture("exit 0");
    let (out, result) = run(
        &repo,
        &from,
        "0123456789012345678901234567890123456789",
        &[],
        &[],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("git worktree add failed"),
        "{}",
        show(&out)
    );
    assert_eq!(result["phase"], "tool-failed");
    assert_ne!(result["exit"], 0);
}

#[test]
fn signal_death_is_tool_failure() {
    let (repo, from, to) = fixture("kill -TERM $$");
    let (out, result) = run(&repo, &from, &to, &[], &[]);
    assert!(!out.status.success(), "{}", show(&out));
    assert_eq!(result["phase"], "tool-failed");
    assert_ne!(result["exit"], 0);
}

#[test]
fn missing_runner_is_tool_failure() {
    let (repo, from, to) = fixture("exit 0");
    let out = repo.jj(&[
        "config",
        "set",
        "--repo",
        "jj-hooks.runner-bin.hk",
        "\"/nonexistent/runner\"",
    ]);
    assert!(out.status.success(), "{}", show(&out));
    let (out, result) = run(&repo, &from, &to, &[], &[]);
    assert!(!out.status.success());
    assert_eq!(result["phase"], "tool-failed");
    assert_ne!(result["exit"], 0);
}

#[test]
fn retry_heals_but_preserves_fixup_evidence() {
    let (repo, from, to) = fixture(
        "if test \"$(cat change.txt)\" = changed; then echo fixed > change.txt; exit 1; fi\nexit 0",
    );
    let (out, result) = run(&repo, &from, &to, &[], &[]);
    assert!(!out.status.success(), "{}", show(&out));
    assert_eq!(result["phase"], "fixup");
    assert!(String::from_utf8_lossy(&out.stderr).contains("re-run on fixup commit was clean"));
    assert_eq!(result["exit"], 0);
    assert_fixup(&repo, &result);
}

#[test]
fn no_retry_keeps_first_fixup() {
    let (repo, from, to) = fixture("echo fixed > change.txt; exit 1");
    let (out, result) = run(&repo, &from, &to, &["--no-retry-after-fixup"], &[]);
    assert!(!out.status.success());
    assert_eq!(result["phase"], "fixup");
    assert_eq!(result["exit"], 0);
    assert_fixup(&repo, &result);
}

#[test]
fn invalid_endpoints_are_rejected() {
    let (repo, from, to) = fixture("exit 0");
    for args in [
        vec!["run", "--from", &from],
        vec!["run", "--from", "main", "--to", &to],
        vec!["run", "--from", &from, "--to", "@"],
    ] {
        assert!(!repo.jj_hooks(&args).status.success());
    }
}

#[test]
fn unwritable_result_prevents_runner() {
    let (repo, from, to) = fixture("touch \"$MARKER\"");
    let marker = repo.tmp.path().join("marker");
    let result = repo.tmp.path().join("missing/result.json");
    let out = repo.jj_hooks_with_env(
        &[
            "run",
            "--from",
            &from,
            "--to",
            &to,
            "--result-json",
            result.to_str().unwrap(),
        ],
        &[("MARKER", marker.to_str().unwrap())],
    );
    assert!(!out.status.success());
    assert!(!marker.exists());
}

#[test]
fn stale_result_is_replaced_on_early_error() {
    let (repo, from, to) = fixture("exit 0");
    let result = repo.tmp.path().join("result.json");
    std::fs::write(&result, "stale").unwrap();
    let out = repo.jj_hooks_in(
        repo.tmp.path(),
        &[
            "run",
            "--from",
            &from,
            "--to",
            &to,
            "--result-json",
            result.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    let value: Value = serde_json::from_slice(&std::fs::read(result).unwrap()).unwrap();
    assert_eq!(value["phase"], "tool-failed");
}

#[test]
fn post_runner_fixup_failure_is_not_runner_evidence() {
    let (repo, from, to) = fixture(
        "index=$(git rev-parse --git-path index); rm -f \"$index\"; mkdir \"$index\"; exit 0",
    );
    let (out, result) = run(&repo, &from, &to, &[], &[]);
    assert!(!out.status.success(), "{}", show(&out));
    assert_eq!(result["phase"], "tool-failed");
    assert_ne!(result["exit"], 0);
}

#[test]
fn signal_killed_setup_is_tool_failure() {
    let (repo, from, to) = fixture("exit 0");
    let out = repo.jj(&[
        "config",
        "set",
        "--repo",
        "jj-hooks.setup",
        r#"[{ name = "killed", run = ["sh", "-c", "kill -TERM $$"] }]"#,
    ]);
    assert!(out.status.success(), "{}", show(&out));
    let (out, result) = run(&repo, &from, &to, &[], &[]);
    assert!(!out.status.success(), "{}", show(&out));
    assert_eq!(result["phase"], "tool-failed");
    assert_ne!(result["exit"], 0);
}

#[test]
fn parse_error_removes_stale_result() {
    let (repo, from, to) = fixture("exit 0");
    let path = repo.tmp.path().join("result.json");
    for args in [
        vec!["run", "--from", "invalid", "--to", &to],
        vec!["run", "--from", &from],
        vec!["run", "--from", &from.to_uppercase(), "--to", &to],
    ] {
        std::fs::write(&path, "stale").unwrap();
        let mut args = args;
        args.extend(["--result-json", path.to_str().unwrap()]);
        let out = repo.jj_hooks(&args);
        assert!(!out.status.success());
        assert!(!path.exists());
    }
}
