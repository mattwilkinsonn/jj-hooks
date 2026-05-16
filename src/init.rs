//! `jj-hooks init` — interactive setup for the user-level config.
//!
//! Three yes/no prompts:
//! 1. Install a `jj push` alias that delegates to `jj-hooks push`.
//! 2. Auto-advance bookmarks when hooks modify files.
//! 3. Install jjui actions/bindings so `jj-hp push` is reachable from
//!    inside [jjui](https://github.com/idursun/jjui).
//!
//! The first two write to the user-level jj config via `jj config set`.
//! The third merges into `~/.config/jjui/config.toml` (or the path passed
//! via `JJUI_CONFIG_DIR`). Prompts go through the [`Prompter`] trait so
//! tests can script answers.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{JjHooksError, Result};
use crate::runner::Runner;

pub trait Prompter {
    fn confirm(&mut self, message: &str, default: bool) -> Result<bool>;
}

/// A prompter that returns pre-canned answers in order. Used in tests.
pub struct ScriptedPrompter {
    answers: std::vec::IntoIter<bool>,
}

impl ScriptedPrompter {
    pub fn new(answers: Vec<bool>) -> Self {
        Self {
            answers: answers.into_iter(),
        }
    }
}

impl Prompter for ScriptedPrompter {
    fn confirm(&mut self, _message: &str, default: bool) -> Result<bool> {
        Ok(self.answers.next().unwrap_or(default))
    }
}

/// Interactive prompter backed by dialoguer.
pub struct InteractivePrompter;

impl Prompter for InteractivePrompter {
    fn confirm(&mut self, message: &str, default: bool) -> Result<bool> {
        dialoguer::Confirm::new()
            .with_prompt(message)
            .default(default)
            .interact()
            .map_err(|e| JjHooksError::Io(std::io::Error::other(e.to_string())))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitPlan {
    pub install_alias: bool,
    pub advance_bookmarks: bool,
    pub install_jjui_actions: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AddedItems {
    pub added_jj_push: bool,
    pub added_jj_push_selected: bool,
    pub added_binding_x_p: bool,
    pub added_binding_x_p_caps: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitOutcome {
    pub alias_set: bool,
    pub advance_bookmarks_set: bool,
    pub jjui_actions_added: AddedItems,
}

/// Build an [`InitPlan`] by asking the user (via `prompter`) which optional
/// integrations to install. `detected_runner` is for informational printing
/// — the plan itself is the same regardless.
pub fn plan(detected_runner: Option<Runner>, prompter: &mut dyn Prompter) -> Result<InitPlan> {
    if let Some(runner) = detected_runner {
        tracing::info!("detected hook runner: {}", runner.bin());
    } else {
        tracing::info!("no hook-runner config detected at workspace root");
    }

    let install_alias = prompter.confirm(
        "Set up `jj push` alias so it runs hooks before pushing?",
        false,
    )?;
    let advance_bookmarks = prompter.confirm(
        "Auto-advance bookmarks to fixup commits when hooks modify files?",
        false,
    )?;
    let install_jjui_actions = prompter.confirm(
        "Install jjui actions/bindings so `jj-hp push` is reachable from inside jjui?",
        false,
    )?;

    Ok(InitPlan {
        install_alias,
        advance_bookmarks,
        install_jjui_actions,
    })
}

/// Apply an [`InitPlan`] by invoking `jj config set --user` for each
/// requested jj key, and merging jjui actions/bindings into the jjui
/// config file when requested.
///
/// - `jj_config_path`: if `Some`, `JJ_CONFIG` is set to that path for the
///   subprocess so writes are scoped (used in tests). `None` writes to
///   the real user config.
/// - `jjui_config_path`: where to merge the jjui actions/bindings. `None`
///   resolves to the canonical path (`$JJUI_CONFIG_DIR/config.toml` or
///   `~/.config/jjui/config.toml`).
pub fn apply(
    plan: &InitPlan,
    jj_config_path: Option<&Path>,
    jjui_config_path: Option<&Path>,
) -> Result<InitOutcome> {
    let mut outcome = InitOutcome {
        alias_set: false,
        advance_bookmarks_set: false,
        jjui_actions_added: AddedItems::default(),
    };

    if plan.install_alias {
        jj_config_set(
            "aliases.push",
            r#"["util", "exec", "--", "jj-hp", "push"]"#,
            jj_config_path,
        )?;
        outcome.alias_set = true;
    }

    if plan.advance_bookmarks {
        jj_config_set("jj-hooks.advance-bookmarks", "true", jj_config_path)?;
        outcome.advance_bookmarks_set = true;
    }

    if plan.install_jjui_actions {
        let path = match jjui_config_path {
            Some(p) => p.to_path_buf(),
            None => default_jjui_config_path()?,
        };
        outcome.jjui_actions_added = apply_jjui_config(&path)?;
    }

    Ok(outcome)
}

fn jj_config_set(key: &str, value: &str, config_path: Option<&Path>) -> Result<()> {
    let mut cmd = Command::new("jj");
    cmd.args(["config", "set", "--user", key, value]);

    if let Some(path) = config_path {
        cmd.env("JJ_CONFIG", path);
    }

    let output = cmd.output()?;
    if !output.status.success() {
        return Err(JjHooksError::JjFailed {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

/// Resolve the canonical jjui config path: `$JJUI_CONFIG_DIR/config.toml`
/// if set, otherwise `$XDG_CONFIG_HOME/jjui/config.toml`, otherwise
/// `~/.config/jjui/config.toml`.
fn default_jjui_config_path() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("JJUI_CONFIG_DIR") {
        return Ok(PathBuf::from(dir).join("config.toml"));
    }
    let base = if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
    } else {
        let home = std::env::var_os("HOME").ok_or_else(|| {
            JjHooksError::Io(std::io::Error::other(
                "neither JJUI_CONFIG_DIR, XDG_CONFIG_HOME, nor HOME is set",
            ))
        })?;
        PathBuf::from(home).join(".config")
    };
    Ok(base.join("jjui").join("config.toml"))
}

/// Read the jjui config at `path` (treating a missing file as empty),
/// merge in our actions/bindings, and write back. Returns which items
/// were added (none if everything was already present).
fn apply_jjui_config(path: &Path) -> Result<AddedItems> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };

    let (merged, added) = add_jjui_actions(&existing)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, merged)?;
    Ok(added)
}

/// Merge `jj-hp-push` / `jj-hp-push-selected` actions and their `x p` / `x P`
/// bindings into a jjui config TOML string.
///
/// Three cases per action:
/// 1. Neither old nor new name present → add the new one.
/// 2. New name already present → leave alone (idempotent).
/// 3. Old `jj-push` / `jj-push-selected` name present with a lua body
///    matching one of our known auto-installed forms → rename in place
///    (action name + any bindings pointing at it, plus binding desc).
/// 4. Old name present with a custom lua body the user wrote → leave
///    alone. The user owns that name.
///
/// Returns the new TOML and a record of which items were newly added.
/// Renames count as 0 added (they're migrations, not adds).
pub fn add_jjui_actions(existing: &str) -> Result<(String, AddedItems)> {
    let mut doc: toml::Table = if existing.trim().is_empty() {
        toml::Table::new()
    } else {
        existing
            .parse()
            .map_err(|e: toml::de::Error| JjHooksError::Parse(format!("jjui config: {e}")))?
    };

    let mut added = AddedItems::default();

    // -- Actions ------------------------------------------------------------
    let actions = doc
        .entry("actions")
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let actions_arr = actions
        .as_array_mut()
        .ok_or_else(|| JjHooksError::Parse("jjui config: `actions` is not an array".into()))?;

    let push_state = classify_action(actions_arr, NEW_PUSH_NAME, OLD_PUSH_NAME, &push_lua_forms());
    let push_selected_state = classify_action(
        actions_arr,
        NEW_PUSH_SELECTED_NAME,
        OLD_PUSH_SELECTED_NAME,
        &push_selected_lua_forms(),
    );

    apply_action(
        actions_arr,
        push_state,
        NEW_PUSH_NAME,
        push_lua_forms()[0].to_owned(),
        &mut added.added_jj_push,
    );
    apply_action(
        actions_arr,
        push_selected_state,
        NEW_PUSH_SELECTED_NAME,
        push_selected_lua_forms()[0].to_owned(),
        &mut added.added_jj_push_selected,
    );

    // -- Bindings -----------------------------------------------------------
    let bindings = doc
        .entry("bindings")
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let bindings_arr = bindings
        .as_array_mut()
        .ok_or_else(|| JjHooksError::Parse("jjui config: `bindings` is not an array".into()))?;

    // For each binding pointing at an old action name, rename its `action`
    // field to the new name and update `desc`. (This is independent of
    // whether the action itself got renamed — there could be a stale
    // binding referencing an action we already renamed.)
    for b in bindings_arr.iter_mut() {
        let Some(action) = b.get("action").and_then(|v| v.as_str()) else {
            continue;
        };
        if action == OLD_PUSH_NAME && push_state == ActionState::OldManaged {
            let table = b.as_table_mut().unwrap();
            table.insert("action".into(), toml::Value::String(NEW_PUSH_NAME.into()));
            table.insert("desc".into(), toml::Value::String(NEW_PUSH_DESC.into()));
        } else if action == OLD_PUSH_SELECTED_NAME && push_selected_state == ActionState::OldManaged
        {
            let table = b.as_table_mut().unwrap();
            table.insert(
                "action".into(),
                toml::Value::String(NEW_PUSH_SELECTED_NAME.into()),
            );
            table.insert(
                "desc".into(),
                toml::Value::String(NEW_PUSH_SELECTED_DESC.into()),
            );
        }
    }

    // Add any missing bindings (idempotent).
    if !bindings_has_action(bindings_arr, NEW_PUSH_NAME) {
        bindings_arr.push(make_binding(
            NEW_PUSH_NAME,
            &["x", "p"],
            "revisions",
            NEW_PUSH_DESC,
        ));
        added.added_binding_x_p = true;
    }
    if !bindings_has_action(bindings_arr, NEW_PUSH_SELECTED_NAME) {
        bindings_arr.push(make_binding(
            NEW_PUSH_SELECTED_NAME,
            &["x", "P"],
            "revisions",
            NEW_PUSH_SELECTED_DESC,
        ));
        added.added_binding_x_p_caps = true;
    }

    let serialized = toml::to_string_pretty(&doc)
        .map_err(|e| JjHooksError::Parse(format!("serializing jjui config: {e}")))?;

    Ok((serialized, added))
}

const NEW_PUSH_NAME: &str = "jj-hp-push";
const NEW_PUSH_SELECTED_NAME: &str = "jj-hp-push-selected";
const OLD_PUSH_NAME: &str = "jj-push";
const OLD_PUSH_SELECTED_NAME: &str = "jj-push-selected";
const NEW_PUSH_DESC: &str = "jj-hp push";
const NEW_PUSH_SELECTED_DESC: &str = "jj-hp push selected bookmark(s)";

/// Known lua bodies we have auto-installed for `jj-hp-push` historically.
/// Index 0 is the current form; later indices are older forms we still
/// recognize as ours (so we can safely rename them).
fn push_lua_forms() -> Vec<&'static str> {
    vec![
        // Current form.
        "  jj_async(\"util\", \"exec\", \"--\", \"jj-hp\", \"push\")\n  revisions.refresh()\n",
        // Pre-jj-hp form (called `jj push` via the alias).
        "  jj_async(\"push\")\n  revisions.refresh()\n",
    ]
}

fn push_selected_lua_forms() -> Vec<&'static str> {
    vec![
        // Current form.
        "  jj_async(\"util\", \"exec\", \"--\", \"jj-hp\", \"push\", \"-r\", context.commit_id())\n  revisions.refresh()\n",
        // Pre-jj-hp form.
        "  jj_async(\"push\", \"-r\", context.commit_id())\n  revisions.refresh()\n",
    ]
}

/// What we found when classifying an action by its name + lua body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionState {
    /// Neither name present anywhere — we need to add the new action.
    Missing,
    /// New name already present — leave alone.
    AlreadyNewName,
    /// Old name present, lua matches one of our known forms — safe to rename.
    OldManaged,
    /// Old name present, lua is custom (user-owned) — leave alone.
    OldUserOwned,
}

fn classify_action(
    actions: &[toml::Value],
    new_name: &str,
    old_name: &str,
    known_lua: &[&str],
) -> ActionState {
    let mut found_new = false;
    let mut found_old: Option<&str> = None;
    for a in actions {
        let Some(name) = a.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        if name == new_name {
            found_new = true;
        }
        if name == old_name {
            found_old = a.get("lua").and_then(|v| v.as_str());
        }
    }
    if found_new {
        return ActionState::AlreadyNewName;
    }
    match found_old {
        None => ActionState::Missing,
        Some(lua) if known_lua.contains(&lua) => ActionState::OldManaged,
        Some(_) => ActionState::OldUserOwned,
    }
}

fn apply_action(
    actions: &mut Vec<toml::Value>,
    state: ActionState,
    new_name: &str,
    new_lua: String,
    added_flag: &mut bool,
) {
    match state {
        ActionState::Missing | ActionState::OldUserOwned => {
            // Either we own the slot and need to add it, or the user has
            // co-opted the old name with custom lua — in both cases we
            // want to add (or skip if user-owned and we have no slot).
            if matches!(state, ActionState::Missing) {
                let mut t = toml::Table::new();
                t.insert("name".into(), toml::Value::String(new_name.into()));
                t.insert("lua".into(), toml::Value::String(new_lua));
                actions.push(toml::Value::Table(t));
                *added_flag = true;
            }
        }
        ActionState::AlreadyNewName => {
            // Idempotent: leave alone.
        }
        ActionState::OldManaged => {
            // Rename in place. Find the old entry and rename it; also
            // refresh the lua to the current form.
            for a in actions.iter_mut() {
                let Some(table) = a.as_table_mut() else {
                    continue;
                };
                let name = table
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_owned());
                let old_match = match name.as_deref() {
                    Some("jj-push") if new_name == NEW_PUSH_NAME => true,
                    Some("jj-push-selected") if new_name == NEW_PUSH_SELECTED_NAME => true,
                    _ => false,
                };
                if old_match {
                    table.insert("name".into(), toml::Value::String(new_name.into()));
                    table.insert("lua".into(), toml::Value::String(new_lua));
                    break;
                }
            }
        }
    }
}

fn bindings_has_action(arr: &[toml::Value], action: &str) -> bool {
    arr.iter()
        .any(|v| v.get("action").and_then(|n| n.as_str()) == Some(action))
}

fn make_binding(action: &str, seq: &[&str], scope: &str, desc: &str) -> toml::Value {
    let mut t = toml::Table::new();
    t.insert("action".into(), toml::Value::String(action.into()));
    t.insert(
        "seq".into(),
        toml::Value::Array(
            seq.iter()
                .map(|s| toml::Value::String((*s).into()))
                .collect(),
        ),
    );
    t.insert("scope".into(), toml::Value::String(scope.into()));
    t.insert("desc".into(), toml::Value::String(desc.into()));
    toml::Value::Table(t)
}
