#![cfg(target_os = "linux")]

//! Signals sent to jj-hp while a hook worktree is live: cleanup runs, children stop.

mod harness;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use harness::{PRE_PUSH_PASSING, PRE_PUSH_SLEEPER, TestRepo, show};

const SIGHUP: i32 = 1;
const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;

const PUSH_ARGS: [&str; 5] = ["--runner", "pre-commit", "push", "-b", "main"];

/// The `PRE_PUSH_SLEEPER` body, for a setup step.
const SLEEPER_SCRIPT: &str = r#"set -- $(cat /proc/$$/stat); printf "%s" "$PWD" > "$JJ_HOOKS_TEST_CWD_OUT"; printf "%s" "$5" > "$JJ_HOOKS_TEST_PGRP_OUT"; (while :; do date > "$JJ_HOOKS_TEST_TICK_OUT"; sleep 0.1; done) & echo "$!" > "$JJ_HOOKS_TEST_PID_OUT"; wait"#;

const PRE_PUSH_RECORD_SESSION: &str = r#"
repos:
  - repo: local
    hooks:
      - id: session
        name: session
        entry: sh -c 'set -- $(cat /proc/$$/stat); printf "%s %s" "$5" "$6" > "$JJ_HOOKS_TEST_PGRP_OUT"; while :; do sleep 0.1; done'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;

const PRE_PUSH_WAIT_FOR_GO: &str = r#"
repos:
  - repo: local
    hooks:
      - id: go
        name: go
        entry: sh -c 'printf "%s" "$PWD" > "$JJ_HOOKS_TEST_CWD_OUT"; while [ ! -e "$JJ_HOOKS_TEST_GO" ]; do sleep 0.1; done'
        language: system
        stages: [pre-push]
        always_run: true
        pass_filenames: false
"#;

struct Fixture {
    repo: TestRepo,
    root: PathBuf,
    log: PathBuf,
    remote_main: Option<String>,
}

impl Fixture {
    fn new(config: &str) -> Self {
        let repo = TestRepo::new();
        repo.write_pre_commit_config(config);
        repo.write("new.txt", "x\n");
        let out = repo.jj(&["commit", "-m", "advance"]);
        assert!(out.status.success(), "{}", show(&out));
        let out = repo.jj(&["bookmark", "set", "main", "-r", "@-"]);
        assert!(out.status.success(), "{}", show(&out));
        let root = repo.tmp.path().join("worktrees");
        std::fs::create_dir(&root).unwrap();
        let log = repo.tmp.path().join("jj-hp.log");
        let remote_main = repo.remote_commit("main");
        Self {
            repo,
            root,
            log,
            remote_main,
        }
    }

    fn out(&self, name: &str) -> PathBuf {
        self.repo.tmp.path().join(name)
    }

    fn set_setup(&self, script: &str) {
        let value = format!(r#"[{{ run = ["sh", "-c", '{script}'] }}]"#);
        let out = self
            .repo
            .jj(&["config", "set", "--repo", "jj-hooks.setup", &value]);
        assert!(out.status.success(), "{}", show(&out));
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        let path = |name: &str| self.out(name).to_str().unwrap().to_owned();
        vec![
            ("JJ_HOOKS_WORKTREE_ROOT", self.root.to_str().unwrap().into()),
            ("JJ_HOOKS_TEST_CWD_OUT", path("cwd")),
            ("JJ_HOOKS_TEST_PGRP_OUT", path("pgrp")),
            ("JJ_HOOKS_TEST_PID_OUT", path("pid")),
            ("JJ_HOOKS_TEST_TICK_OUT", path("tick")),
            ("JJ_HOOKS_TEST_SETUP_OUT", path("setup")),
            ("JJ_HOOKS_TEST_GO", path("go")),
        ]
    }

    fn spawn(&self, wrapper: &[&str]) -> Child {
        let env = self.env();
        let env: Vec<_> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.repo
            .jj_hooks_spawn_wrapped(wrapper, &PUSH_ARGS, &env, &self.log)
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn wait_for(&self, names: &[&str]) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !names.iter().all(|name| has_content(&self.out(name))) {
            assert!(
                Instant::now() < deadline,
                "{names:?} not written; log: {}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.out(name))
            .unwrap()
            .trim()
            .to_owned()
    }

    fn wait_exit(&self, child: &mut Child, within: Duration) -> ExitStatus {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("jj-hp did not exit within {within:?}; log: {}", self.log());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn assert_worktrees_gone(&self) {
        let left: Vec<_> = std::fs::read_dir(&self.root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(
            left.is_empty(),
            "root not empty: {left:?}; log: {}",
            self.log()
        );
        let output = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(self.repo.primary())
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", show(&output));
        let listing = String::from_utf8(output.stdout).unwrap();
        let paths: Vec<_> = listing
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .collect();
        assert_eq!(paths, [self.repo.primary().to_str().unwrap()], "{listing}");
    }

    fn assert_nothing_pushed(&self) {
        assert_eq!(self.repo.remote_commit("main"), self.remote_main);
        assert!(
            self.repo
                .refs_matching("refs/heads/jj-hooks-fixup/*")
                .is_empty()
        );
    }

    fn assert_gone(&self, name: &str) {
        let pid = self.read(name);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !process_gone(&pid) {
            assert!(Instant::now() < deadline, "process {pid} ({name}) survived");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Never leak a test's background loop past a failure.
        if let Ok(pid) = std::fs::read_to_string(self.out("pid")) {
            let _ = Command::new("kill")
                .args(["-9", pid.trim()])
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn has_content(path: &std::path::Path) -> bool {
    std::fs::read(path).is_ok_and(|contents| !contents.is_empty())
}

fn process_gone(pid: &str) -> bool {
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    stat.is_empty() || stat.starts_with('Z')
}

fn send(signal: i32, target: &str) {
    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg("--")
        .arg(target)
        .status()
        .unwrap();
    assert!(status.success(), "kill -{signal} {target} failed");
}

fn assert_died_by(fixture: &Fixture, status: ExitStatus, signal: i32) {
    assert_eq!(
        status.signal(),
        Some(signal),
        "jj-hp exit {status}; log: {}",
        fixture.log()
    );
}

fn signal_during_sleeper(fixture: &Fixture, signal: i32) {
    let mut child = fixture.spawn(&[]);
    fixture.wait_for(&["cwd", "pgrp", "pid", "tick"]);
    assert!(
        fixture
            .read("cwd")
            .starts_with(fixture.root.to_str().unwrap())
    );
    assert_ne!(
        fixture.read("pgrp"),
        child.id().to_string(),
        "the child ran in jj-hp's own group"
    );

    send(signal, &child.id().to_string());
    let status = fixture.wait_exit(&mut child, Duration::from_secs(10));
    assert_died_by(fixture, status, signal);
    fixture.assert_gone("pid");
    fixture.assert_worktrees_gone();
    fixture.assert_nothing_pushed();
}

#[test]
fn sigterm_stops_hook_group_and_removes_worktree() {
    let fixture = Fixture::new(PRE_PUSH_SLEEPER);
    signal_during_sleeper(&fixture, SIGTERM);
}

#[test]
fn sigint_stops_hook_group_and_removes_worktree() {
    let fixture = Fixture::new(PRE_PUSH_SLEEPER);
    signal_during_sleeper(&fixture, SIGINT);
}

#[test]
fn sigterm_stops_captured_setup_step_and_removes_worktree() {
    let fixture = Fixture::new(PRE_PUSH_PASSING);
    fixture.set_setup(SLEEPER_SCRIPT);
    signal_during_sleeper(&fixture, SIGTERM);
}

#[test]
fn sigterm_wakes_reader_blocked_on_term_ignoring_grandchild() {
    let fixture = Fixture::new(PRE_PUSH_PASSING);
    fixture.set_setup(
        r#"(trap "" TERM; while :; do sleep 0.1; done) & echo "$!" > "$JJ_HOOKS_TEST_PID_OUT"; exit 0"#,
    );
    let mut child = fixture.spawn(&[]);
    fixture.wait_for(&["pid"]);

    send(SIGTERM, &child.id().to_string());
    let status = fixture.wait_exit(&mut child, Duration::from_secs(10));
    assert_died_by(&fixture, status, SIGTERM);
    fixture.assert_gone("pid");
    fixture.assert_worktrees_gone();
    fixture.assert_nothing_pushed();
}

#[test]
fn second_sigterm_kills_term_ignoring_setup_step() {
    let fixture = Fixture::new(PRE_PUSH_PASSING);
    fixture.set_setup(
        r#"trap "" TERM; echo "$$" > "$JJ_HOOKS_TEST_PID_OUT"; while :; do sleep 0.1; done"#,
    );
    let mut child = fixture.spawn(&[]);
    fixture.wait_for(&["pid"]);
    let jj_hp = child.id().to_string();

    send(SIGTERM, &jj_hp);
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        child.try_wait().unwrap().is_none(),
        "jj-hp exited on the first SIGTERM; log: {}",
        fixture.log()
    );
    assert!(
        !process_gone(&fixture.read("pid")),
        "the TERM-ignoring step died on the first SIGTERM"
    );

    send(SIGTERM, &jj_hp);
    let status = fixture.wait_exit(&mut child, Duration::from_secs(10));
    assert_died_by(&fixture, status, SIGTERM);
    fixture.assert_gone("pid");
    fixture.assert_worktrees_gone();
    fixture.assert_nothing_pushed();
}

#[test]
fn sigterm_during_stalled_worktree_add_removes_worktree() {
    let fixture = Fixture::new(PRE_PUSH_PASSING);
    let hooks = fixture.out("git-hooks");
    std::fs::create_dir(&hooks).unwrap();
    let hook = hooks.join("post-checkout");
    std::fs::write(
        &hook,
        "#!/bin/sh\necho \"$$\" > \"$JJ_HOOKS_TEST_PID_OUT\"\nwhile :; do sleep 0.1; done\n",
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    harness::run(
        fixture.repo.primary(),
        "git",
        &[
            "config",
            "--local",
            "core.hooksPath",
            hooks.to_str().unwrap(),
        ],
    );

    let mut child = fixture.spawn(&[]);
    fixture.wait_for(&["pid"]);
    send(SIGTERM, &child.id().to_string());
    let status = fixture.wait_exit(&mut child, Duration::from_secs(10));
    assert_died_by(&fixture, status, SIGTERM);
    fixture.assert_gone("pid");
    fixture.assert_worktrees_gone();
    fixture.assert_nothing_pushed();
}

#[test]
fn terminal_ctrl_c_keeps_children_in_foreground_group_and_cleans_up() {
    let fixture = Fixture::new(PRE_PUSH_RECORD_SESSION);
    fixture.set_setup(
        r#"set -- $(cat /proc/$$/stat); printf "%s %s" "$5" "$6" > "$JJ_HOOKS_TEST_SETUP_OUT""#,
    );
    let command = format!(
        "exec '{}' {}",
        env!("CARGO_BIN_EXE_jj-hp"),
        PUSH_ARGS.join(" ")
    );
    let log = std::fs::File::create(&fixture.log).unwrap();
    let mut script = Command::new("script");
    script
        .args(["-qec", &command, "/dev/null"])
        .current_dir(fixture.repo.primary())
        .env("PRE_COMMIT_HOME", &fixture.repo.pre_commit_home)
        .env("JJ_HOOKS_LOG", "info")
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log));
    for (key, value) in fixture.env() {
        script.env(key, value);
    }
    let mut child = script.spawn().unwrap();
    fixture.wait_for(&["pgrp", "setup"]);

    let hook = fixture.read("pgrp");
    let setup = fixture.read("setup");
    for pair in [&hook, &setup] {
        let (pgrp, session) = pair.split_once(' ').unwrap();
        assert_eq!(pgrp, session, "child left the terminal's foreground group");
    }
    assert_eq!(hook, setup);

    let (pgrp, _) = hook.split_once(' ').unwrap();
    send(SIGINT, &format!("-{pgrp}"));
    let status = fixture.wait_exit(&mut child, Duration::from_secs(10));
    assert_eq!(status.code(), Some(130), "log: {}", fixture.log());
    fixture.assert_worktrees_gone();
    fixture.assert_nothing_pushed();
}

#[test]
fn inherited_ignored_sighup_stays_ignored() {
    let fixture = Fixture::new(PRE_PUSH_WAIT_FOR_GO);
    let mut child = fixture.spawn(&["sh", "-c", r#"trap "" HUP; exec "$0" "$@""#]);
    fixture.wait_for(&["cwd"]);

    send(SIGHUP, &child.id().to_string());
    std::thread::sleep(Duration::from_millis(200));
    std::fs::write(fixture.out("go"), "go").unwrap();
    let status = fixture.wait_exit(&mut child, Duration::from_secs(30));
    assert!(status.success(), "{status}; log: {}", fixture.log());
    fixture.assert_worktrees_gone();
}
