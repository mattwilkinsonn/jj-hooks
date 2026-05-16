use jj_hooks::init::{self, AddedItems, InitOutcome, InitPlan, ScriptedPrompter, add_jjui_actions};
use jj_hooks::runner::Runner;
use std::path::PathBuf;

#[test]
fn plan_with_all_yes() {
    let mut prompter = ScriptedPrompter::new(vec![true, true, true]);
    let plan = init::plan(Some(Runner::PreCommit), &mut prompter).unwrap();
    assert_eq!(
        plan,
        InitPlan {
            install_alias: true,
            advance_bookmarks: true,
            install_jjui_actions: true,
        }
    );
}

#[test]
fn plan_with_all_no() {
    let mut prompter = ScriptedPrompter::new(vec![false, false, false]);
    let plan = init::plan(Some(Runner::Lefthook), &mut prompter).unwrap();
    assert_eq!(
        plan,
        InitPlan {
            install_alias: false,
            advance_bookmarks: false,
            install_jjui_actions: false,
        }
    );
}

#[test]
fn plan_mixed() {
    let mut prompter = ScriptedPrompter::new(vec![true, false, true]);
    let plan = init::plan(Some(Runner::Hk), &mut prompter).unwrap();
    assert_eq!(
        plan,
        InitPlan {
            install_alias: true,
            advance_bookmarks: false,
            install_jjui_actions: true,
        }
    );
}

#[test]
fn plan_with_no_runner_detected_still_prompts() {
    let mut prompter = ScriptedPrompter::new(vec![true, true, true]);
    let plan = init::plan(None, &mut prompter).unwrap();
    assert_eq!(
        plan,
        InitPlan {
            install_alias: true,
            advance_bookmarks: true,
            install_jjui_actions: true,
        }
    );
}

#[test]
fn apply_writes_expected_config_keys() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path: PathBuf = tmp.path().join("config.toml");
    std::fs::write(&config_path, "").unwrap();

    let plan = InitPlan {
        install_alias: true,
        advance_bookmarks: true,
        install_jjui_actions: false,
    };
    let outcome = init::apply(&plan, Some(&config_path), None).unwrap();
    assert_eq!(
        outcome,
        InitOutcome {
            alias_set: true,
            advance_bookmarks_set: true,
            jjui_actions_added: AddedItems::default(),
        }
    );

    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        contents.contains(r#"push = ["util", "exec", "--", "jj-hp", "push"]"#),
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
        install_jjui_actions: false,
    };
    let outcome = init::apply(&plan, Some(&config_path), None).unwrap();
    assert_eq!(
        outcome,
        InitOutcome {
            alias_set: false,
            advance_bookmarks_set: false,
            jjui_actions_added: AddedItems::default(),
        }
    );

    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !contents.contains("jj-hooks"),
        "should be empty:\n{contents}"
    );
}

#[test]
fn add_jjui_actions_to_empty_config() {
    let (output, added) = add_jjui_actions("").unwrap();
    assert!(added.added_jj_push);
    assert!(added.added_jj_push_selected);
    assert!(added.added_binding_x_p);
    assert!(added.added_binding_x_p_caps);

    // Re-parse so we don't depend on the pretty-printer's array layout.
    let parsed: toml::Table = output.parse().unwrap();
    let actions = parsed["actions"].as_array().unwrap();
    let action_names: Vec<&str> = actions
        .iter()
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
        .collect();
    assert!(action_names.contains(&"jj-hp-push"), "{action_names:?}");
    assert!(
        action_names.contains(&"jj-hp-push-selected"),
        "{action_names:?}"
    );

    let bindings = parsed["bindings"].as_array().unwrap();
    let mut found_xp = false;
    let mut found_xp_caps = false;
    let mut xp_desc = "";
    let mut xp_caps_desc = "";
    for b in bindings {
        let action = b.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let seq: Vec<&str> = b
            .get("seq")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let desc = b.get("desc").and_then(|v| v.as_str()).unwrap_or("");
        if action == "jj-hp-push" && seq == ["x", "p"] {
            found_xp = true;
            xp_desc = desc;
        }
        if action == "jj-hp-push-selected" && seq == ["x", "P"] {
            found_xp_caps = true;
            xp_caps_desc = desc;
        }
    }
    assert!(found_xp, "expected jj-hp-push bound to x p");
    assert!(found_xp_caps, "expected jj-hp-push-selected bound to x P");
    assert_eq!(xp_desc, "jj-hp push");
    assert_eq!(xp_caps_desc, "jj-hp push selected bookmark(s)");

    // The lua bodies should invoke jj-hp directly.
    let lua_bodies: Vec<&str> = actions
        .iter()
        .filter_map(|v| v.get("lua").and_then(|l| l.as_str()))
        .collect();
    for lua in &lua_bodies {
        assert!(
            lua.contains("jj-hp"),
            "lua body should call jj-hp directly:\n{lua}"
        );
        assert!(
            !lua.contains("jj_async(\"push\""),
            "lua should not depend on the `jj push` alias:\n{lua}"
        );
    }
}

#[test]
fn add_jjui_actions_idempotent_on_second_run() {
    let (first, _) = add_jjui_actions("").unwrap();
    let (second, added) = add_jjui_actions(&first).unwrap();

    assert!(!added.added_jj_push);
    assert!(!added.added_jj_push_selected);
    assert!(!added.added_binding_x_p);
    assert!(!added.added_binding_x_p_caps);

    let parsed: toml::Table = second.parse().unwrap();
    let actions = parsed["actions"].as_array().unwrap();
    let count = actions
        .iter()
        .filter(|v| v.get("name").and_then(|n| n.as_str()) == Some("jj-hp-push"))
        .count();
    assert_eq!(count, 1);
}

#[test]
fn add_jjui_actions_preserves_existing_user_actions() {
    let existing = r#"
[[actions]]
name = "my-custom"
lua = "print('hi')"

[[bindings]]
action = "my-custom"
seq = ["q"]
scope = "revisions"
desc = "quit"
"#;
    let (output, added) = add_jjui_actions(existing).unwrap();

    assert!(added.added_jj_push);
    assert!(output.contains(r#"name = "my-custom""#), "{output}");
    assert!(output.contains(r#"["q"]"#), "{output}");
    assert!(output.contains(r#"name = "jj-hp-push""#), "{output}");
}

#[test]
fn add_jjui_actions_keeps_user_owned_jj_push_when_name_already_taken() {
    // User has *their own* action literally named "jj-push" with a custom
    // lua body. We must not rename or clobber it.
    let existing = r#"
[[actions]]
name = "jj-push"
lua = "print('user version')"
"#;
    let (output, added) = add_jjui_actions(existing).unwrap();
    assert!(
        !added.added_jj_push,
        "should not have added (user already has one with custom lua)"
    );
    assert!(output.contains("print('user version')"));
    // jj-hp-push-selected should still get added since its name is free.
    assert!(added.added_jj_push_selected);
    assert!(output.contains(r#"name = "jj-hp-push-selected""#));
}

#[test]
fn add_jjui_actions_renames_old_managed_jj_push_to_jj_hp_push() {
    // Existing config has the OLD action/binding names but lua bodies
    // we know we wrote (i.e. they're auto-installed, not user-customized).
    // Expected: rename `jj-push` → `jj-hp-push`, rename the binding's
    // `action` reference and update its `desc`.
    let existing = r#"
[[actions]]
name = "jj-push"
lua = """
  jj_async("util", "exec", "--", "jj-hp", "push")
  revisions.refresh()
"""

[[actions]]
name = "jj-push-selected"
lua = """
  jj_async("util", "exec", "--", "jj-hp", "push", "-r", context.commit_id())
  revisions.refresh()
"""

[[bindings]]
action = "jj-push"
seq = ["x", "p"]
scope = "revisions"
desc = "jj push"

[[bindings]]
action = "jj-push-selected"
seq = ["x", "P"]
scope = "revisions"
desc = "jj push selected bookmark(s)"
"#;
    let (output, added) = add_jjui_actions(existing).unwrap();

    // Nothing was "added" — everything was renamed in place.
    assert!(!added.added_jj_push, "should be a rename, not an add");
    assert!(
        !added.added_jj_push_selected,
        "should be a rename, not an add"
    );

    let parsed: toml::Table = output.parse().unwrap();
    let action_names: Vec<&str> = parsed["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
        .collect();
    assert!(
        action_names.contains(&"jj-hp-push"),
        "expected rename to jj-hp-push: {action_names:?}"
    );
    assert!(
        action_names.contains(&"jj-hp-push-selected"),
        "expected rename to jj-hp-push-selected: {action_names:?}"
    );
    assert!(
        !action_names.contains(&"jj-push"),
        "old name should be gone: {action_names:?}"
    );

    // Bindings should have been rewired to the new action name AND
    // their descs updated.
    let bindings = parsed["bindings"].as_array().unwrap();
    let mut found_xp = false;
    let mut found_xp_caps = false;
    for b in bindings {
        let action = b.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let desc = b.get("desc").and_then(|v| v.as_str()).unwrap_or("");
        if action == "jj-hp-push" {
            found_xp = true;
            assert_eq!(desc, "jj-hp push", "binding desc not updated");
        }
        if action == "jj-hp-push-selected" {
            found_xp_caps = true;
            assert_eq!(
                desc, "jj-hp push selected bookmark(s)",
                "binding desc not updated"
            );
        }
        assert_ne!(action, "jj-push", "stale binding action reference");
        assert_ne!(action, "jj-push-selected", "stale binding action reference");
    }
    assert!(found_xp);
    assert!(found_xp_caps);
}

#[test]
fn apply_writes_jjui_config_when_requested() {
    let tmp = tempfile::TempDir::new().unwrap();
    let jj_config = tmp.path().join("jj-config.toml");
    let jjui_config = tmp.path().join("jjui-config.toml");
    std::fs::write(&jj_config, "").unwrap();

    let plan = InitPlan {
        install_alias: false,
        advance_bookmarks: false,
        install_jjui_actions: true,
    };
    let outcome = init::apply(&plan, Some(&jj_config), Some(&jjui_config)).unwrap();
    assert!(outcome.jjui_actions_added.added_jj_push);
    assert!(outcome.jjui_actions_added.added_binding_x_p);

    let written = std::fs::read_to_string(&jjui_config).unwrap();
    assert!(written.contains(r#"name = "jj-hp-push""#));
}
