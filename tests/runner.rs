use jj_hooks::runner::{
    Runner, Stage, hook_command, hook_command_all_files, lefthook_command,
    lefthook_command_all_files, prefer_prek_when_available,
};
use tempfile::TempDir;

#[test]
fn pre_commit_pre_push() {
    let cmd = hook_command(Runner::PreCommit, Stage::PrePush, "old", "new");
    assert_eq!(
        cmd,
        vec![
            "pre-commit",
            "run",
            "--hook-stage",
            "pre-push",
            "--from-ref",
            "old",
            "--to-ref",
            "new",
        ]
    );
}

#[test]
fn pre_commit_pre_commit() {
    let cmd = hook_command(Runner::PreCommit, Stage::PreCommit, "old", "new");
    assert_eq!(
        cmd,
        vec![
            "pre-commit",
            "run",
            "--hook-stage",
            "pre-commit",
            "--from-ref",
            "old",
            "--to-ref",
            "new",
        ]
    );
}

#[test]
fn prek_pre_push() {
    let cmd = hook_command(Runner::Prek, Stage::PrePush, "old", "new");
    assert_eq!(cmd[0], "prek");
    assert!(cmd.contains(&"--hook-stage".to_string()));
    assert!(cmd.contains(&"pre-push".to_string()));
}

#[test]
fn hk_pre_push() {
    let cmd = hook_command(Runner::Hk, Stage::PrePush, "old", "new");
    assert_eq!(
        cmd,
        vec![
            "hk",
            "run",
            "pre-push",
            "--from-ref",
            "old",
            "--to-ref",
            "new",
        ]
    );
}

#[test]
fn hk_pre_commit() {
    let cmd = hook_command(Runner::Hk, Stage::PreCommit, "old", "new");
    assert_eq!(
        cmd,
        vec![
            "hk",
            "run",
            "pre-commit",
            "--from-ref",
            "old",
            "--to-ref",
            "new",
        ]
    );
}

#[test]
fn lefthook_pre_push_with_files() {
    let cmd = lefthook_command(
        Stage::PrePush,
        &["src/main.rs".into(), "tests/parse.rs".into()],
    );
    assert_eq!(
        cmd,
        vec![
            "lefthook",
            "run",
            "pre-push",
            "--file",
            "src/main.rs",
            "--file",
            "tests/parse.rs",
        ]
    );
}

#[test]
fn lefthook_pre_commit_no_files() {
    // Empty file list still produces a valid command — lefthook handles "nothing to do" itself.
    let cmd = lefthook_command(Stage::PreCommit, &[]);
    assert_eq!(cmd, vec!["lefthook", "run", "pre-commit"]);
}

// --- --all-files command builders -------------------------------------------
//
// `--all-files` swaps each runner's ref/file selection for the
// runner's own "ignore the diff, run on the whole tree" mode.
// One unit test per runner pins the exact argv we send.

#[test]
fn hook_command_all_files_pre_commit() {
    let cmd = hook_command_all_files(Runner::PreCommit, Stage::PrePush);
    assert_eq!(
        cmd,
        vec![
            "pre-commit",
            "run",
            "--hook-stage",
            "pre-push",
            "--all-files",
        ],
    );
    assert!(
        !cmd.iter().any(|a| a == "--from-ref" || a == "--to-ref"),
        "--all-files mode must not pass --from-ref/--to-ref",
    );
}

#[test]
fn hook_command_all_files_prek() {
    let cmd = hook_command_all_files(Runner::Prek, Stage::PrePush);
    assert_eq!(cmd[0], "prek");
    assert!(cmd.iter().any(|a| a == "--all-files"));
    assert!(!cmd.iter().any(|a| a == "--from-ref" || a == "--to-ref"));
}

#[test]
fn hook_command_all_files_hk() {
    // hk's `-a/--all` doesn't actually override ref bounds on stage
    // hooks (despite what `hk run --help` says) — `--glob '*'` is
    // the one flag that replaces the file selection. Verified with
    // hk 1.45.0 in /tmp; see runner.rs docstring for details.
    let cmd = hook_command_all_files(Runner::Hk, Stage::PrePush);
    assert_eq!(cmd, vec!["hk", "run", "pre-push", "--glob", "*"]);
    assert!(!cmd.iter().any(|a| a == "--from-ref" || a == "--to-ref"));
}

#[test]
#[should_panic(expected = "lefthook")]
fn hook_command_all_files_lefthook_panics() {
    // Symmetric to hook_command — lefthook needs its own builder
    // because the all-files form replaces the per-file selection
    // rather than the ref bounds.
    let _ = hook_command_all_files(Runner::Lefthook, Stage::PrePush);
}

#[test]
fn lefthook_command_all_files_emits_all_files_flag() {
    let cmd = lefthook_command_all_files(Stage::PrePush);
    assert_eq!(cmd, vec!["lefthook", "run", "pre-push", "--all-files"]);
    assert!(
        !cmd.iter().any(|a| a == "--file"),
        "--all-files must not also emit --file selections",
    );
}

#[test]
fn stage_display() {
    assert_eq!(Stage::PrePush.as_str(), "pre-push");
    assert_eq!(Stage::PreCommit.as_str(), "pre-commit");
}

#[test]
fn autodetect_none() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(Runner::autodetect(tmp.path()).unwrap(), None);
}

#[test]
fn autodetect_lefthook() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lefthook.yml"), "").unwrap();
    assert_eq!(
        Runner::autodetect(tmp.path()).unwrap(),
        Some(Runner::Lefthook)
    );
}

#[test]
fn autodetect_lefthook_dotted_variant() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".lefthook.yaml"), "").unwrap();
    assert_eq!(
        Runner::autodetect(tmp.path()).unwrap(),
        Some(Runner::Lefthook)
    );
}

#[test]
fn autodetect_lefthook_toml() {
    // Regression for issue #285: lefthook supports TOML config in
    // addition to YAML, but autodetect only looked for the YAML
    // variants — a repo with only `lefthook.toml` reported "no
    // hook-runner config" and silently skipped hooks.
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lefthook.toml"), "").unwrap();
    assert_eq!(
        Runner::autodetect(tmp.path()).unwrap(),
        Some(Runner::Lefthook)
    );
}

#[test]
fn autodetect_lefthook_dotted_toml() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".lefthook.toml"), "").unwrap();
    assert_eq!(
        Runner::autodetect(tmp.path()).unwrap(),
        Some(Runner::Lefthook)
    );
}

#[test]
fn autodetect_lefthook_json() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lefthook.json"), "").unwrap();
    assert_eq!(
        Runner::autodetect(tmp.path()).unwrap(),
        Some(Runner::Lefthook)
    );
}

#[test]
fn autodetect_lefthook_jsonc() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lefthook.jsonc"), "").unwrap();
    assert_eq!(
        Runner::autodetect(tmp.path()).unwrap(),
        Some(Runner::Lefthook)
    );
}

#[test]
fn autodetect_lefthook_config_dir() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".config")).unwrap();
    std::fs::write(root.join(".config/lefthook.yml"), "").unwrap();
    assert_eq!(Runner::autodetect(root).unwrap(), Some(Runner::Lefthook));
}

#[test]
fn autodetect_lefthook_config_dir_toml() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".config")).unwrap();
    std::fs::write(root.join(".config/lefthook.toml"), "").unwrap();
    assert_eq!(Runner::autodetect(root).unwrap(), Some(Runner::Lefthook));
}

#[test]
fn autodetect_pre_commit() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".pre-commit-config.yaml"), "").unwrap();
    assert_eq!(
        Runner::autodetect(tmp.path()).unwrap(),
        Some(Runner::PreCommit)
    );
}

#[test]
fn autodetect_hk() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("hk.pkl"), "").unwrap();
    assert_eq!(Runner::autodetect(tmp.path()).unwrap(), Some(Runner::Hk));
}

#[test]
fn autodetect_prek_native_toml() {
    // Regression for issue #17: a repo with only `prek.toml` (prek's
    // native config, not the pre-commit YAML) used to autodetect as
    // None and silently skip hooks. It should be picked up as Prek.
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("prek.toml"), "").unwrap();
    assert_eq!(Runner::autodetect(tmp.path()).unwrap(), Some(Runner::Prek));
}

#[test]
fn autodetect_prek_native_dotted_toml() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".prek.toml"), "").unwrap();
    assert_eq!(Runner::autodetect(tmp.path()).unwrap(), Some(Runner::Prek));
}

#[test]
fn autodetect_prek_collapses_with_pre_commit_yaml() {
    // prek consumes both `prek.toml` and `.pre-commit-config.yaml`. A
    // repo with both shouldn't trip the "multiple hook-runner configs"
    // ambiguity error — it's the same runner family. Collapse to Prek.
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("prek.toml"), "").unwrap();
    std::fs::write(tmp.path().join(".pre-commit-config.yaml"), "").unwrap();
    assert_eq!(Runner::autodetect(tmp.path()).unwrap(), Some(Runner::Prek));
}

#[test]
fn autodetect_ambiguous_errors() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lefthook.yml"), "").unwrap();
    std::fs::write(tmp.path().join(".pre-commit-config.yaml"), "").unwrap();
    let err = Runner::autodetect(tmp.path()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("multiple"), "{msg}");
    assert!(msg.contains("lefthook"), "{msg}");
    assert!(msg.contains("pre-commit"), "{msg}");
}

#[test]
fn prefer_prek_swaps_pre_commit_when_available() {
    assert_eq!(
        prefer_prek_when_available(Runner::PreCommit, true),
        Runner::Prek
    );
}

#[test]
fn prefer_prek_leaves_pre_commit_when_prek_missing() {
    assert_eq!(
        prefer_prek_when_available(Runner::PreCommit, false),
        Runner::PreCommit
    );
}

#[test]
fn prefer_prek_does_not_swap_lefthook() {
    assert_eq!(
        prefer_prek_when_available(Runner::Lefthook, true),
        Runner::Lefthook
    );
}

#[test]
fn prefer_prek_does_not_swap_hk() {
    assert_eq!(prefer_prek_when_available(Runner::Hk, true), Runner::Hk);
}

#[test]
fn prefer_prek_is_idempotent_on_prek() {
    assert_eq!(prefer_prek_when_available(Runner::Prek, true), Runner::Prek);
    assert_eq!(
        prefer_prek_when_available(Runner::Prek, false),
        Runner::Prek
    );
}
