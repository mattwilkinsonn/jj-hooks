use jj_hooks::runner::{Runner, Stage, hook_command, lefthook_command, prefer_prek_when_available};
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
