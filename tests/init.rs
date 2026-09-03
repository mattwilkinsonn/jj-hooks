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
        // Post-2026-05 swap: jj-hp-push-selected (the common case)
        // takes lowercase `x p`; jj-hp-push (push entire stack)
        // takes uppercase `x P`.
        if action == "jj-hp-push-selected" && seq == ["x", "p"] {
            found_xp = true;
            xp_desc = desc;
        }
        if action == "jj-hp-push" && seq == ["x", "P"] {
            found_xp_caps = true;
            xp_caps_desc = desc;
        }
    }
    assert!(found_xp, "expected jj-hp-push-selected bound to x p");
    assert!(found_xp_caps, "expected jj-hp-push bound to x P");
    assert_eq!(xp_desc, "jj-hp push selected bookmark");
    assert_eq!(xp_caps_desc, "jj-hp push every bookmark");

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

    // The `jj-hp-push` (uppercase, every-bookmark) action MUST
    // include the `--all` flag. A regression to the bare
    // `jj-hp push` form would still pass the desc assertion
    // above as long as the binding's `desc` string also drifted
    // back; this body-level check fails on the underlying
    // semantic change, not just the displayed label.
    let push_action_lua = actions
        .iter()
        .find(|v| v.get("name").and_then(|n| n.as_str()) == Some("jj-hp-push"))
        .and_then(|v| v.get("lua").and_then(|l| l.as_str()))
        .expect("jj-hp-push action should exist");
    assert!(
        push_action_lua.contains("\"--all\""),
        "jj-hp-push lua body should include `--all`; got: {push_action_lua}"
    );
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
            assert_eq!(
                desc, "jj-hp push every bookmark",
                "binding desc not updated"
            );
        }
        if action == "jj-hp-push-selected" {
            found_xp_caps = true;
            assert_eq!(
                desc, "jj-hp push selected bookmark",
                "binding desc not updated"
            );
        }
        assert_ne!(action, "jj-push", "stale binding action reference");
        assert_ne!(action, "jj-push-selected", "stale binding action reference");
    }
    assert!(found_xp);
    assert!(found_xp_caps);

    // The renamed `jj-hp-push` action's lua should also have been
    // refreshed forward to the `--all` form. Without this body
    // assertion a future regression that renames the action but
    // forgets to refresh the lua (or vice versa) would pass.
    let push_action_lua = parsed["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v.get("name").and_then(|n| n.as_str()) == Some("jj-hp-push"))
        .and_then(|v| v.get("lua").and_then(|l| l.as_str()))
        .expect("jj-hp-push action should exist after rename");
    assert!(
        push_action_lua.contains("\"--all\""),
        "renamed jj-hp-push lua should include `--all`; got: {push_action_lua}"
    );
}

#[test]
fn add_jjui_actions_preserves_user_customized_desc_on_old_managed_rename() {
    // Regression for the PR #74 review: the OLD→NEW name rename
    // used to blindly overwrite `desc`. If a user kept the
    // managed lua (so the action is still classified as
    // `OldManaged`) but edited the binding's display text, that
    // edit was silently clobbered. The fix: only refresh `desc`
    // when its existing value is in the managed history set
    // (i.e. one we've installed ourselves).
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
desc = "My custom push label"

[[bindings]]
action = "jj-push-selected"
seq = ["x", "P"]
scope = "revisions"
desc = "Push just this one"
"#;
    let (output, _added) = add_jjui_actions(existing).unwrap();
    let parsed: toml::Table = output.parse().unwrap();
    let bindings = parsed["bindings"].as_array().unwrap();

    let mut saw_push = false;
    let mut saw_push_selected = false;
    for b in bindings {
        let action = b.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let desc = b.get("desc").and_then(|v| v.as_str()).unwrap_or("");
        if action == "jj-hp-push" {
            saw_push = true;
            assert_eq!(
                desc, "My custom push label",
                "user-customized desc must survive the rename; got {desc}"
            );
        }
        if action == "jj-hp-push-selected" {
            saw_push_selected = true;
            assert_eq!(
                desc, "Push just this one",
                "user-customized desc must survive the rename; got {desc}"
            );
        }
    }
    assert!(saw_push, "renamed jj-hp-push binding should exist");
    assert!(
        saw_push_selected,
        "renamed jj-hp-push-selected binding should exist"
    );
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

#[test]
fn add_jjui_actions_swaps_managed_seq_from_pre_swap_to_post_swap() {
    // Pre-2026-05 config: jj-hp-push was bound to lowercase x p
    // and jj-hp-push-selected to uppercase x P. The migration
    // must swap them so jj-hp-push-selected (the common case)
    // gets the easier keypress.
    let existing = r#"
[[actions]]
name = "jj-hp-push"
lua = """
  jj_async("util", "exec", "--", "jj-hp", "push")
  revisions.refresh()
"""

[[actions]]
name = "jj-hp-push-selected"
lua = """
  jj_async("util", "exec", "--", "jj-hp", "push", "-r", context.commit_id())
  revisions.refresh()
"""

[[bindings]]
action = "jj-hp-push"
seq = ["x", "p"]
scope = "revisions"
desc = "jj-hp push"

[[bindings]]
action = "jj-hp-push-selected"
seq = ["x", "P"]
scope = "revisions"
desc = "jj-hp push selected bookmark(s)"
"#;
    let (output, _added) = add_jjui_actions(existing).unwrap();
    let parsed: toml::Table = output.parse().unwrap();

    let bindings = parsed["bindings"].as_array().unwrap();
    let mut push_seq: Option<Vec<String>> = None;
    let mut selected_seq: Option<Vec<String>> = None;
    for b in bindings {
        let action = b.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let seq: Vec<String> = b
            .get("seq")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        if action == "jj-hp-push" {
            push_seq = Some(seq);
        } else if action == "jj-hp-push-selected" {
            selected_seq = Some(seq);
        }
    }
    assert_eq!(
        push_seq.as_deref(),
        Some(&["x".to_owned(), "P".to_owned()][..])
    );
    assert_eq!(
        selected_seq.as_deref(),
        Some(&["x".to_owned(), "p".to_owned()][..])
    );
}

#[test]
fn add_jjui_actions_refreshes_stale_lua_and_desc_when_seq_already_current() {
    // The "AlreadyNewNameStaleLua + migrate_binding_desc" path:
    // the user already has `jj-hp-push` at the current seq
    // (`x P`) but with pre-2026-06 lua (no `--all`) and the old
    // pre-broaden desc. `migrate_seq_for_managed_binding` should
    // NOT fire (seq matches current, not historical), but the
    // lua refresh + desc rewrite should still land. This pins
    // the migration's "desc-only" path independently of the
    // seq-swap path that the swap_is_idempotent test exercises.
    let stale_lua = r#"
[[actions]]
name = "jj-hp-push"
lua = """
  jj_async("util", "exec", "--", "jj-hp", "push")
  revisions.refresh()
"""

[[bindings]]
action = "jj-hp-push"
seq = ["x", "P"]
scope = "revisions"
desc = "jj-hp push"
"#;
    let (output, _) = add_jjui_actions(stale_lua).unwrap();
    let parsed: toml::Table = output.parse().unwrap();

    // Action's lua should now include --all.
    let actions = parsed["actions"].as_array().unwrap();
    let push_action = actions
        .iter()
        .find(|a| a.get("name").and_then(|v| v.as_str()) == Some("jj-hp-push"))
        .expect("jj-hp-push action should still exist");
    let lua = push_action.get("lua").and_then(|v| v.as_str()).unwrap();
    assert!(
        lua.contains("\"--all\""),
        "stale lua should have been refreshed to include --all; got: {lua}",
    );

    // Binding's seq should be unchanged (already current); desc
    // should have been refreshed.
    let bindings = parsed["bindings"].as_array().unwrap();
    let push_binding = bindings
        .iter()
        .find(|b| b.get("action").and_then(|v| v.as_str()) == Some("jj-hp-push"))
        .unwrap();
    let seq: Vec<&str> = push_binding["seq"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(seq, vec!["x", "P"], "seq should be untouched");
    let desc = push_binding.get("desc").and_then(|v| v.as_str()).unwrap();
    assert_eq!(
        desc, "jj-hp push every bookmark",
        "desc should have been refreshed to the post-2026-06 wording"
    );
}

#[test]
fn add_jjui_actions_seq_swap_is_idempotent() {
    // Running twice on the same input should not flip the
    // sequences back. The post-swap state is at the head of
    // the seq-history list; the migrate-prior detection
    // doesn't fire.
    let pre_swap = r#"
[[actions]]
name = "jj-hp-push"
lua = """
  jj_async("util", "exec", "--", "jj-hp", "push")
  revisions.refresh()
"""

[[bindings]]
action = "jj-hp-push"
seq = ["x", "p"]
scope = "revisions"
desc = "jj-hp push"
"#;
    let (first, _) = add_jjui_actions(pre_swap).unwrap();
    let (second, _) = add_jjui_actions(&first).unwrap();
    assert_eq!(first, second, "second run should be a no-op");

    // The first run should have swapped the seq.
    let parsed: toml::Table = first.parse().unwrap();
    let bindings = parsed["bindings"].as_array().unwrap();
    let push_binding = bindings
        .iter()
        .find(|b| b.get("action").and_then(|v| v.as_str()) == Some("jj-hp-push"))
        .unwrap();
    let seq: Vec<&str> = push_binding["seq"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(seq, vec!["x", "P"]);
}

#[test]
fn add_jjui_actions_does_not_swap_user_customized_seq() {
    // The user picked their own key sequence for jj-hp-push.
    // The migration must leave it alone because it's not in
    // our installed-history list.
    let user_custom = r#"
[[actions]]
name = "jj-hp-push"
lua = """
  jj_async("util", "exec", "--", "jj-hp", "push")
  revisions.refresh()
"""

[[bindings]]
action = "jj-hp-push"
seq = ["g", "p", "u"]
scope = "revisions"
desc = "my custom push key"
"#;
    let (output, _) = add_jjui_actions(user_custom).unwrap();
    let parsed: toml::Table = output.parse().unwrap();
    let bindings = parsed["bindings"].as_array().unwrap();
    let push_binding = bindings
        .iter()
        .find(|b| b.get("action").and_then(|v| v.as_str()) == Some("jj-hp-push"))
        .unwrap();
    let seq: Vec<&str> = push_binding["seq"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(seq, vec!["g", "p", "u"], "user's custom key was clobbered");
    // The desc should also be preserved — the migration's desc
    // refresh only runs when the seq was actually a prior value.
    let desc = push_binding
        .get("desc")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(desc, "my custom push key");
}

#[test]
fn add_jjui_actions_does_not_swap_seq_when_action_has_custom_lua() {
    // The harder case: action name `jj-hp-push` AND seq `x p`
    // both match historical managed values, but the action's
    // lua body is the user's hand-written code (not in our
    // installed-history). Without the managed-lua gate,
    // `migrate_seq_for_managed_binding` would silently rewrite
    // the seq + desc to our current shape, relabeling the user's
    // custom action as the shipped one.
    //
    // Pin the contract: a user-owned action keeps its seq +
    // desc + lua even when name & seq incidentally line up.
    let custom_lua = r#"
[[actions]]
name = "jj-hp-push"
lua = """
  -- user's hand-written code
  jj_async("util", "exec", "--", "jj-hp", "push", "--draft", "--remote", "fork")
  revisions.refresh()
"""

[[bindings]]
action = "jj-hp-push"
seq = ["x", "p"]
scope = "revisions"
desc = "jj-hp push"
"#;
    let (output, _) = add_jjui_actions(custom_lua).unwrap();
    let parsed: toml::Table = output.parse().unwrap();
    let bindings = parsed["bindings"].as_array().unwrap();
    let push_binding = bindings
        .iter()
        .find(|b| b.get("action").and_then(|v| v.as_str()) == Some("jj-hp-push"))
        .unwrap();
    let seq: Vec<&str> = push_binding["seq"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        seq,
        vec!["x", "p"],
        "user-owned action's seq must not be migrated"
    );

    // The action's lua body must stay the user's custom one.
    let actions = parsed["actions"].as_array().unwrap();
    let action = actions
        .iter()
        .find(|a| a.get("name").and_then(|v| v.as_str()) == Some("jj-hp-push"))
        .unwrap();
    let lua = action.get("lua").and_then(|v| v.as_str()).unwrap();
    assert!(
        lua.contains("--draft") && lua.contains("--remote"),
        "user's custom lua body should be preserved; got: {lua}",
    );
}

#[test]
fn add_jjui_actions_orders_by_frequency() {
    // The menu order matters: jjui's `x`-prefix overlay surfaces
    // candidates top-down in the order they appear in the config.
    // Selected-bookmark push (the daily case) sits at index 0 so
    // the muscle-memory `x p` keystroke is the shortest path
    // through the menu; whole-stack push (`x P`) is rarer and
    // sits below. Pin both the action and binding order so a
    // future reshuffle has to update this list AND the swap in
    // src/init.rs.
    let (output, _) = add_jjui_actions("").unwrap();
    let parsed: toml::Table = output.parse().unwrap();
    let action_order: Vec<&str> = parsed["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
        .collect();
    assert_eq!(
        action_order,
        vec![
            "jj-hp-push-selected", // daily: push focused bookmark only
            "jj-hp-push",          // whole-stack push (less common)
        ],
        "action order drifted from selected-first frequency layout",
    );
    let binding_order: Vec<&str> = parsed["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.get("action").and_then(|n| n.as_str()))
        .collect();
    assert_eq!(
        binding_order,
        vec!["jj-hp-push-selected", "jj-hp-push"],
        "binding order drifted from selected-first frequency layout",
    );
}

#[test]
fn readme_toml_matches_generated_jjui_config() {
    // Drift guard: the README's jjui-integration TOML block must
    // mirror what `add_jjui_actions("")` produces, including the
    // ORDER of actions and bindings. Ordering matters because
    // jjui's `x`-prefix overlay surfaces candidates in the order
    // they appear in the config; reshuffling the swap in
    // src/init.rs without updating the README would silently
    // produce different menu sort orders for the two install
    // paths (auto via `jj-hooks init` vs hand-paste from README).
    //
    // The README has multiple ```toml blocks (config snippets for
    // other features); we grab the FIRST one because that's the
    // one the new section adds. Future shuffles that move the
    // jjui block past index 0 will need to update this test.
    let readme_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let readme = std::fs::read_to_string(&readme_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", readme_path.display()));

    let start_marker = "```toml\n";
    let start = readme
        .find(start_marker)
        .expect("no ```toml block in README");
    let body_start = start + start_marker.len();
    let end = readme[body_start..]
        .find("\n```")
        .expect("toml block has no closing fence");
    let readme_toml = &readme[body_start..body_start + end];

    let readme_parsed: toml::Table = readme_toml
        .parse()
        .unwrap_or_else(|e| panic!("parse README TOML: {e}\n---\n{readme_toml}\n---"));

    let readme_action_order: Vec<&str> = readme_parsed
        .get("actions")
        .and_then(|v| v.as_array())
        .expect("README TOML has no [[actions]]")
        .iter()
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
        .collect();
    let readme_binding_order: Vec<(&str, Vec<&str>)> = readme_parsed
        .get("bindings")
        .and_then(|v| v.as_array())
        .expect("README TOML has no [[bindings]]")
        .iter()
        .filter_map(|v| {
            let action = v.get("action").and_then(|n| n.as_str())?;
            let seq: Vec<&str> = v
                .get("seq")
                .and_then(|n| n.as_array())?
                .iter()
                .filter_map(|s| s.as_str())
                .collect();
            Some((action, seq))
        })
        .collect();

    let (generated, _) = add_jjui_actions("").unwrap();
    let generated_parsed: toml::Table = generated.parse().unwrap();
    let generated_action_order: Vec<&str> = generated_parsed["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
        .collect();
    let generated_binding_order: Vec<(&str, Vec<&str>)> = generated_parsed["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| {
            let action = v.get("action").and_then(|n| n.as_str())?;
            let seq: Vec<&str> = v
                .get("seq")
                .and_then(|n| n.as_array())?
                .iter()
                .filter_map(|s| s.as_str())
                .collect();
            Some((action, seq))
        })
        .collect();

    assert_eq!(
        readme_action_order, generated_action_order,
        "README action ORDER drifted from generated config (jjui menu sort \
         order will differ between auto-install and copy/paste)",
    );
    assert_eq!(
        readme_binding_order, generated_binding_order,
        "README binding ORDER drifted from generated config",
    );
}

#[test]
fn add_jjui_actions_refreshes_lua_and_desc_when_seq_already_current() {
    // The headline migration path that the 2026-06 `--all` rollout
    // depends on: a user whose config already has `jj-hp-push`
    // bound to the current `x P` seq + the pre-2026-06 bareword
    // lua (no `--all`) + the pre-2026-06 desc (`"jj-hp push"`).
    // None of the seq-migration helpers fire (seq is already
    // current), so the lua refresh (via AlreadyNewNameStaleLua)
    // and the desc rewrite (via migrate_binding_desc) are the
    // only paths that touch the config.
    //
    // Pinning this end-to-end matters because the pre-2026-05-swap
    // idempotency test (`add_jjui_actions_seq_swap_is_idempotent`)
    // uses `x p` for the selected binding, so seq migration runs
    // there and masks any standalone desc-refresh bug. Without
    // this test a regression in the desc-refresh-only path would
    // ship silently.
    let pre_all_form = r#"
[[actions]]
name = "jj-hp-push"
lua = """
  jj_async("util", "exec", "--", "jj-hp", "push")
  revisions.refresh()
"""

[[bindings]]
action = "jj-hp-push"
seq = ["x", "P"]
scope = "revisions"
desc = "jj-hp push"
"#;
    let (output, added) = add_jjui_actions(pre_all_form).unwrap();
    // The `jj-hp-push` (everything) action and binding both
    // already existed, so neither should have been added by the
    // helper. The selected counterpart (`jj-hp-push-selected`)
    // wasn't in the fixture, so `added_jj_push_selected` and
    // `added_binding_x_p_caps` will be true — that's outside
    // this test's scope.
    assert!(
        !added.added_jj_push,
        "everything action should not have been added"
    );
    assert!(
        !added.added_binding_x_p,
        "everything binding should not have been added"
    );

    let parsed: toml::Table = output.parse().unwrap();

    // Lua MUST have been refreshed to include `--all`.
    let lua = parsed["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v.get("name").and_then(|n| n.as_str()) == Some("jj-hp-push"))
        .and_then(|v| v.get("lua").and_then(|l| l.as_str()))
        .expect("jj-hp-push action should still exist");
    assert!(
        lua.contains("\"--all\""),
        "lua should have been refreshed to the --all form; got: {lua}"
    );

    // Binding seq MUST be unchanged (it was already current).
    let binding = parsed["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v.get("action").and_then(|n| n.as_str()) == Some("jj-hp-push"))
        .expect("jj-hp-push binding should exist");
    let seq: Vec<&str> = binding["seq"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(seq, vec!["x", "P"], "seq should not have moved");

    // Desc MUST have been refreshed to the new wording.
    let desc = binding.get("desc").and_then(|v| v.as_str()).unwrap();
    assert_eq!(desc, "jj-hp push every bookmark", "desc not refreshed");
}

#[test]
fn add_jjui_actions_leaves_user_customized_jj_hp_push_lua_alone() {
    // Negative case for the managed-action gate in
    // `migrate_binding_desc` (PR 74 feedback): a user who
    // hand-wrote their own lua body for `jj-hp-push` AND happens
    // to have left the old shipped `desc` string in place must
    // NOT have their desc rewritten to the new wording. The
    // desc rewrite is supposed to gate on "the action's lua is
    // still one of ours."
    let custom = r#"
[[actions]]
name = "jj-hp-push"
lua = """
  -- user's custom wrapper that adds env + retries
  os.execute("MY_FLAG=1 jj-hp push --tracked")
  revisions.refresh()
"""

[[bindings]]
action = "jj-hp-push"
seq = ["x", "P"]
scope = "revisions"
desc = "jj-hp push"
"#;
    let (output, _added) = add_jjui_actions(custom).unwrap();
    let parsed: toml::Table = output.parse().unwrap();

    // The user's lua body should be untouched.
    let lua = parsed["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v.get("name").and_then(|n| n.as_str()) == Some("jj-hp-push"))
        .and_then(|v| v.get("lua").and_then(|l| l.as_str()))
        .expect("jj-hp-push action should still exist");
    assert!(
        lua.contains("MY_FLAG=1"),
        "user's custom lua body should be preserved; got: {lua}"
    );

    // Desc should also be untouched — even though it matches our
    // historical string, the action's lua is user-owned so the
    // managed-action gate blocks the rewrite.
    let desc = parsed["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v.get("action").and_then(|n| n.as_str()) == Some("jj-hp-push"))
        .and_then(|v| v.get("desc").and_then(|d| d.as_str()))
        .unwrap();
    assert_eq!(
        desc, "jj-hp push",
        "user-owned action's binding desc must not be rewritten",
    );
}

#[test]
fn add_jjui_actions_classify_and_apply_agree_on_first_match_with_duplicate_entries() {
    // Regression for CodeRabbit's PR 74 feedback: a hand-edited
    // config with two `[[actions]]` entries for the same name
    // ("jj-hp-push") would previously make `classify_action`
    // record the LAST entry's lua while `apply_action` rewrote
    // the FIRST entry. Net effect: classify says "stale managed
    // lua, refresh it" based on the second entry's value, then
    // apply_action overwrites the FIRST entry's bytes — which
    // were the user's hand-written code.
    //
    // Fix: both `classify_action` and `apply_action` now operate
    // on first-match semantics. Either the first entry's lua is
    // managed (refresh it) or it isn't (leave alone). The second
    // entry is irrelevant — duplicate names are user noise we
    // shouldn't read meaning into.
    let dup_first_user_second_managed = r#"
[[actions]]
name = "jj-hp-push"
lua = """
  -- user's custom wrapper, kept FIRST in the file
  os.execute("MY_FLAG=1 jj-hp push --tracked")
  revisions.refresh()
"""

[[actions]]
name = "jj-hp-push"
lua = """
  jj_async("util", "exec", "--", "jj-hp", "push")
  revisions.refresh()
"""

[[bindings]]
action = "jj-hp-push"
seq = ["x", "P"]
scope = "revisions"
desc = "jj-hp push"
"#;
    let (output, _added) = add_jjui_actions(dup_first_user_second_managed).unwrap();
    let parsed: toml::Table = output.parse().unwrap();

    // Find ALL actions with this name (the duplicate stays — we
    // don't dedupe — but neither should we have clobbered the
    // first entry).
    let actions = parsed["actions"].as_array().unwrap();
    let matching: Vec<&toml::Value> = actions
        .iter()
        .filter(|a| a.get("name").and_then(|n| n.as_str()) == Some("jj-hp-push"))
        .collect();
    assert_eq!(
        matching.len(),
        2,
        "duplicate entries should be preserved as-is, not deduped; got {} matches",
        matching.len(),
    );

    // FIRST entry must still be the user's custom wrapper (not
    // overwritten by the apply step). This is the contract the
    // first-match alignment pins.
    let first_lua = matching[0].get("lua").and_then(|v| v.as_str()).unwrap();
    assert!(
        first_lua.contains("MY_FLAG=1"),
        "first-match entry should remain the user's custom lua; got: {first_lua}",
    );
}
