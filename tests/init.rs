use std::path::PathBuf;

use jj_hooks::init::{self, InitOutcome, InitPlan, ScriptedPrompter};
use jj_hooks::runner::Runner;

#[test]
fn plan_with_both_yes() {
    let mut prompter = ScriptedPrompter::new(vec![true, true]);
    let plan = init::plan(Some(Runner::PreCommit), &mut prompter).unwrap();
    assert_eq!(
        plan,
        InitPlan {
            install_alias: true,
            advance_bookmarks: true,
        }
    );
}

#[test]
fn plan_with_both_no() {
    let mut prompter = ScriptedPrompter::new(vec![false, false]);
    let plan = init::plan(Some(Runner::Lefthook), &mut prompter).unwrap();
    assert_eq!(
        plan,
        InitPlan {
            install_alias: false,
            advance_bookmarks: false,
        }
    );
}

#[test]
fn plan_mixed() {
    let mut prompter = ScriptedPrompter::new(vec![true, false]);
    let plan = init::plan(Some(Runner::Hk), &mut prompter).unwrap();
    assert_eq!(
        plan,
        InitPlan {
            install_alias: true,
            advance_bookmarks: false,
        }
    );
}

#[test]
fn plan_with_no_runner_detected_still_prompts() {
    // Even if no runner config is present, init should still let users
    // configure the alias and bookmark behavior — they might be about to
    // add lefthook.yml right after.
    let mut prompter = ScriptedPrompter::new(vec![true, true]);
    let plan = init::plan(None, &mut prompter).unwrap();
    assert_eq!(
        plan,
        InitPlan {
            install_alias: true,
            advance_bookmarks: true,
        }
    );
}

#[test]
fn apply_writes_expected_config_keys() {
    // Use JJ_CONFIG to point at a tempdir-scoped config file so we don't
    // touch the real user config.
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path: PathBuf = tmp.path().join("config.toml");
    std::fs::write(&config_path, "").unwrap();

    // Need a jj repo too — `jj config set --user` works without one, but
    // we test that explicitly via the env var path.
    let plan = InitPlan {
        install_alias: true,
        advance_bookmarks: true,
    };
    let outcome = init::apply(&plan, Some(&config_path)).unwrap();
    assert_eq!(
        outcome,
        InitOutcome {
            alias_set: true,
            advance_bookmarks_set: true,
        }
    );

    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        contents.contains(r#"push = ["util", "exec", "--", "jj-hooks", "push"]"#),
        "alias not written:\n{contents}"
    );
    assert!(
        contents.contains("advance-bookmarks = true"),
        "advance-bookmarks not written:\n{contents}"
    );
}

#[test]
fn apply_skips_when_all_false() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, "").unwrap();

    let plan = InitPlan {
        install_alias: false,
        advance_bookmarks: false,
    };
    let outcome = init::apply(&plan, Some(&config_path)).unwrap();
    assert_eq!(
        outcome,
        InitOutcome {
            alias_set: false,
            advance_bookmarks_set: false,
        }
    );

    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !contents.contains("jj-hooks"),
        "should be empty:\n{contents}"
    );
}
