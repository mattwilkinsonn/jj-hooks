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

    // Selected first: same frequency-ordering rationale as jj-gt's
    // SPECS array. jjui's `x`-prefix overlay surfaces candidates
    // top-down in config order, so the daily "ship the focused
    // bookmark" keystroke should land at index 0. Whole-stack
    // push (`x P`) is the less-common operation and sits below.
    apply_action(
        actions_arr,
        push_selected_state,
        NEW_PUSH_SELECTED_NAME,
        push_selected_lua_forms()[0].to_owned(),
        &mut added.added_jj_push_selected,
    );
    apply_action(
        actions_arr,
        push_state,
        NEW_PUSH_NAME,
        push_lua_forms()[0].to_owned(),
        &mut added.added_jj_push,
    );

    // -- Bindings -----------------------------------------------------------
    //
    // Take a snapshot of the actions array NOW so the binding-
    // migration helpers can consult it for managed-action checks
    // without contending with the mutable borrow we need for
    // `bindings`. Cheap — toml::Value clones are shallow-ish and
    // the list has a handful of entries at most.
    let actions_snapshot: Vec<toml::Value> = actions_arr.clone();
    let bindings = doc
        .entry("bindings")
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let bindings_arr = bindings
        .as_array_mut()
        .ok_or_else(|| JjHooksError::Parse("jjui config: `bindings` is not an array".into()))?;

    // For each binding pointing at an old action name, rename its `action`
    // field to the new name and refresh `desc` IF AND ONLY IF the existing
    // desc is one we've ever installed ourselves (or absent). User-customized
    // desc text is preserved — pinning the binding action name alone is the
    // unsurprising migration; clobbering display text the user typed is not.
    // (This is independent of whether the action itself got renamed — there
    // could be a stale binding referencing an action we already renamed.)
    for b in bindings_arr.iter_mut() {
        let Some(action) = b.get("action").and_then(|v| v.as_str()) else {
            continue;
        };
        if action == OLD_PUSH_NAME && push_state == ActionState::OldManaged {
            let table = b.as_table_mut().unwrap();
            table.insert("action".into(), toml::Value::String(NEW_PUSH_NAME.into()));
            let existing_desc = table.get("desc").and_then(|v| v.as_str());
            if existing_desc.is_none_or(|d| push_desc_managed_set().contains(&d)) {
                table.insert("desc".into(), toml::Value::String(NEW_PUSH_DESC.into()));
            }
        } else if action == OLD_PUSH_SELECTED_NAME && push_selected_state == ActionState::OldManaged
        {
            let table = b.as_table_mut().unwrap();
            table.insert(
                "action".into(),
                toml::Value::String(NEW_PUSH_SELECTED_NAME.into()),
            );
            let existing_desc = table.get("desc").and_then(|v| v.as_str());
            if existing_desc.is_none_or(|d| push_selected_desc_managed_set().contains(&d)) {
                table.insert(
                    "desc".into(),
                    toml::Value::String(NEW_PUSH_SELECTED_DESC.into()),
                );
            }
        }
    }

    // Migrate `seq` for managed bindings whose key sequence matches
    // a previous value we installed but not the current one. The
    // 2026-05 swap moved `jj-hp-push` to `x P` and `jj-hp-push-
    // selected` to `x p`; older configs need updating.
    //
    // Detection key: action name is ours AND `seq` is in our
    // installed-history. Custom keys the user picked are left alone
    // because they're not in our history list.
    migrate_seq_for_managed_binding(
        bindings_arr,
        &actions_snapshot,
        NEW_PUSH_NAME,
        &push_seq_history(),
        NEW_PUSH_DESC,
        &push_lua_forms(),
        push_desc_managed_set(),
    );
    migrate_seq_for_managed_binding(
        bindings_arr,
        &actions_snapshot,
        NEW_PUSH_SELECTED_NAME,
        &push_selected_seq_history(),
        NEW_PUSH_SELECTED_DESC,
        &push_selected_lua_forms(),
        push_selected_desc_managed_set(),
    );

    // Migrate `desc` independently of seq — when an action's lua
    // was updated (e.g. push got `--all` in 2026-06), the binding's
    // desc string also changed but the seq didn't. The seq-
    // migration above won't fire for those configs, so refresh
    // desc here whenever it matches a historical form we shipped.
    //
    // We pass `actions_arr` + the lua-history list so the helper
    // can gate the desc rewrite on "the action's current lua is
    // still one of ours." Without this guard, a user who
    // customized the lua but left the old shipped desc string in
    // place would have their desc silently rewritten to the new
    // wording — quietly mis-labelling their custom action.
    migrate_binding_desc(
        bindings_arr,
        &actions_snapshot,
        NEW_PUSH_NAME,
        NEW_PUSH_DESC,
        &push_desc_history(),
        &push_lua_forms(),
    );
    migrate_binding_desc(
        bindings_arr,
        &actions_snapshot,
        NEW_PUSH_SELECTED_NAME,
        NEW_PUSH_SELECTED_DESC,
        &push_selected_desc_history(),
        &push_selected_lua_forms(),
    );

    // Add any missing bindings (idempotent). Selected-bookmark
    // binding comes first to match the action ordering — keeps
    // jjui's `x`-prefix overlay listing daily-use keys at the top
    // and whole-stack/recovery flows below.
    if !bindings_has_action(bindings_arr, NEW_PUSH_SELECTED_NAME) {
        bindings_arr.push(make_binding(
            NEW_PUSH_SELECTED_NAME,
            &push_selected_seq_history()[0],
            "revisions",
            NEW_PUSH_SELECTED_DESC,
        ));
        added.added_binding_x_p_caps = true;
    }
    if !bindings_has_action(bindings_arr, NEW_PUSH_NAME) {
        bindings_arr.push(make_binding(
            NEW_PUSH_NAME,
            &push_seq_history()[0],
            "revisions",
            NEW_PUSH_DESC,
        ));
        added.added_binding_x_p = true;
    }

    let serialized = toml::to_string_pretty(&doc)
        .map_err(|e| JjHooksError::Parse(format!("serializing jjui config: {e}")))?;

    Ok((serialized, added))
}

const NEW_PUSH_NAME: &str = "jj-hp-push";
const NEW_PUSH_SELECTED_NAME: &str = "jj-hp-push-selected";
const OLD_PUSH_NAME: &str = "jj-push";
const OLD_PUSH_SELECTED_NAME: &str = "jj-push-selected";
const NEW_PUSH_DESC: &str = "jj-hp push every bookmark";
const NEW_PUSH_SELECTED_DESC: &str = "jj-hp push selected bookmark";

/// Every key sequence we have ever installed for `jj-hp-push`,
/// most-recent first. The current value is index 0.
///
/// The 2026-05 swap moved the "push everything" action from
/// `x p` (lowercase) to `x P` (uppercase) so that the more
/// commonly-used `jj-hp-push-selected` could take the easier
/// keypress. `apply_jjui_config`'s migration pass detects the
/// previous sequence and updates it in place when the binding's
/// name is ours and its lua body is still managed.
fn push_seq_history() -> Vec<Vec<&'static str>> {
    vec![
        vec!["x", "P"], // current — push entire @-ancestor stack
        vec!["x", "p"], // pre-2026-05 swap
    ]
}

/// Every key sequence we have ever installed for
/// `jj-hp-push-selected`, most-recent first.
fn push_selected_seq_history() -> Vec<Vec<&'static str>> {
    vec![
        vec!["x", "p"], // current — push just the focused bookmark
        vec!["x", "P"], // pre-2026-05 swap
    ]
}

/// Every binding `desc` we have ever installed for `jj-hp-push`,
/// most-recent first. Used by `migrate_binding_desc` to refresh
/// the displayed text after we change the action's semantics.
fn push_desc_history() -> Vec<&'static str> {
    vec![
        // Current: "every bookmark" reflects the new `--all` lua.
        NEW_PUSH_DESC,
        // Pre-2026-06: bareword `jj-hp push` desc.
        "jj-hp push",
    ]
}

/// Every binding `desc` we have ever installed for the OLD push
/// action name (`jj-push`) before the 2026-06 rename, plus every
/// desc we have ever installed for the new name. Used by the
/// rename-time desc migration to recognise an unmodified
/// managed desc (which we may safely overwrite) vs a
/// user-customized desc (which we must preserve).
///
/// Order doesn't matter — this is membership-tested, not
/// preference-ordered like `push_desc_history`.
fn push_desc_managed_set() -> &'static [&'static str] {
    &[
        // Pre-2026-06 default desc on the old `jj-push` binding.
        "jj push",
        // Every desc we've ever installed for the new name. The
        // current `jj-hp-push` action shouldn't show up on a
        // still-OLD-name binding in practice, but including it
        // keeps the rename idempotent if a user manually edited
        // the action name without touching the desc.
        NEW_PUSH_DESC,
        "jj-hp push",
    ]
}

/// Every binding `desc` we have ever installed for
/// `jj-hp-push-selected`, most-recent first.
fn push_selected_desc_history() -> Vec<&'static str> {
    vec![
        // Current: dropped the "(s)" since the binding really
        // operates on the single focused bookmark.
        NEW_PUSH_SELECTED_DESC,
        // Pre-2026-06 form.
        "jj-hp push selected bookmark(s)",
    ]
}

/// Counterpart to `push_desc_managed_set` for the
/// `jj-hp-push-selected` action — desc strings we may safely
/// overwrite during the OLD→NEW name rename.
fn push_selected_desc_managed_set() -> &'static [&'static str] {
    &[
        // Pre-2026-06 default on the old `jj-push-selected`
        // binding.
        "jj push selected bookmark(s)",
        // Current and intermediate forms.
        NEW_PUSH_SELECTED_DESC,
        "jj-hp push selected bookmark(s)",
    ]
}

/// Known lua bodies we have auto-installed for `jj-hp-push` historically.
/// Index 0 is the current form; later indices are older forms we still
/// recognize as ours (so we can safely rename them).
fn push_lua_forms() -> Vec<&'static str> {
    vec![
        // Current form: `--all` so `x P` pushes every local
        // bookmark across the repo, not just the @-ancestor
        // chain. Mirrors jj-gt's parallel 2026-06 broadening —
        // jjui's cursor moves freely without `@`, so an
        // @-anchored "push everything" key never matched intent.
        "  jj_async(\"util\", \"exec\", \"--\", \"jj-hp\", \"push\", \"--all\")\n  revisions.refresh()\n",
        // Pre-2026-06 form: bareword `jj-hp push` (only the
        // @-default bookmarks).
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
    /// New name already present with current lua — leave alone.
    AlreadyNewName,
    /// New name already present but lua matches a historical form
    /// (other than index 0). Refresh the lua to the current form
    /// without renaming.
    AlreadyNewNameStaleLua,
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
    // Track presence of the new-name action independently of its
    // lua. A managed action SHOULD always have a `lua` field
    // (we put it there), but config can be hand-edited and the
    // `lua` key could be missing. Without this split, the pre-
    // refactor `bool` presence check would have returned
    // `AlreadyNewName` for a lua-less entry and `apply_action`
    // would have left it alone; the lua-Option-only version
    // fell through to the `found_old` arm and could synthesize a
    // duplicate. Restore the presence-first semantics.
    //
    // First-match semantics for both `found_new_lua` and
    // `found_old` so we agree with `apply_action`, which uses
    // `break` after the first match it sees. If a hand-edited
    // config has duplicate entries for the same action name, the
    // last-match-wins behavior we had previously would classify
    // against the LATER entry's lua, then `apply_action` would
    // overwrite the EARLIER entry's lua — potentially clobbering
    // user-owned bytes the user kept in the first slot.
    let mut found_new = false;
    let mut found_new_lua: Option<&str> = None;
    let mut found_old: Option<&str> = None;
    for a in actions {
        let Some(name) = a.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        if name == new_name && !found_new {
            found_new = true;
            found_new_lua = a.get("lua").and_then(|v| v.as_str());
        }
        if name == old_name && found_old.is_none() {
            found_old = a.get("lua").and_then(|v| v.as_str());
        }
    }
    if found_new {
        let Some(lua) = found_new_lua else {
            // New name present but no `lua` field — leave alone
            // rather than duplicate. The action is structurally
            // odd but it's still the user's choice.
            return ActionState::AlreadyNewName;
        };
        // New name present. Refresh the lua if it matches an
        // older form we've installed historically. Custom user
        // lua (anything outside the history list) is left alone.
        let current = known_lua.first().copied().unwrap_or("");
        if lua == current {
            return ActionState::AlreadyNewName;
        }
        if known_lua.contains(&lua) {
            return ActionState::AlreadyNewNameStaleLua;
        }
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
        ActionState::AlreadyNewNameStaleLua => {
            // Same name we installed; lua matches an older form
            // we shipped (in `known_lua` past index 0). Refresh
            // in place — keeps user keybindings intact while
            // picking up the new lua body.
            for a in actions.iter_mut() {
                let Some(table) = a.as_table_mut() else {
                    continue;
                };
                if table.get("name").and_then(|v| v.as_str()) == Some(new_name) {
                    table.insert("lua".into(), toml::Value::String(new_lua));
                    break;
                }
            }
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

/// In-place migration: when a binding's `action` matches `name` and
/// its `seq` is a sequence we previously installed for that action
/// (anything in `seq_history` past index 0), rewrite the `seq` to
/// the current value (index 0) and refresh `desc`.
///
/// User-customized sequences are NOT migrated — they're not in
/// `seq_history`, so the detection falls through. Idempotent: a
/// binding whose seq is already the current value is left alone.
///
/// The `actions` / `lua_history` pair gates the rewrite on
/// "the action this binding points at is still running one of our
/// managed lua forms." Without that guard, a user who hand-wrote
/// a `jj-hp-push` action with custom lua but kept a historical
/// `seq` (e.g. they liked our `x p`/`x P` swap but the lua flag
/// changes broke their workflow, so they reverted lua only) would
/// have their seq AND desc clobbered back to our managed shape.
/// Mirrors the same managed-action check
/// `migrate_binding_desc` uses — both helpers refuse to touch a
/// binding whose action carries non-managed lua.
fn migrate_seq_for_managed_binding(
    bindings: &mut [toml::Value],
    actions: &[toml::Value],
    name: &str,
    seq_history: &[Vec<&'static str>],
    desc: &str,
    lua_history: &[&str],
    managed_desc_set: &[&str],
) {
    if seq_history.len() < 2 {
        return; // No prior versions to migrate from.
    }
    // Look up the action's current lua body once. If it isn't in
    // our installed-history list (current form or any prior),
    // treat the action as user-owned and don't touch ANY of its
    // bindings' seqs — even ones whose seq happens to match a
    // historical form. Same rationale as `migrate_binding_desc`.
    let action_lua = actions
        .iter()
        .find(|a| a.get("name").and_then(|n| n.as_str()) == Some(name))
        .and_then(|a| a.get("lua").and_then(|v| v.as_str()));
    let action_is_managed = match action_lua {
        Some(lua) => lua_history.contains(&lua),
        // No action with this name → no binding can refer to it;
        // bail.
        None => return,
    };
    if !action_is_managed {
        return;
    }
    let current = &seq_history[0];
    let prior: std::collections::HashSet<&[&str]> =
        seq_history[1..].iter().map(|s| s.as_slice()).collect();
    for b in bindings.iter_mut() {
        let Some(action) = b.get("action").and_then(|v| v.as_str()) else {
            continue;
        };
        if action != name {
            continue;
        }
        let Some(existing_seq) = b.get("seq").and_then(|v| v.as_array()) else {
            continue;
        };
        let as_strs: Vec<&str> = existing_seq.iter().filter_map(|v| v.as_str()).collect();
        if !prior.contains(as_strs.as_slice()) {
            // Either the seq is already current, or the user
            // picked a custom key. Either way, no migration.
            continue;
        }
        // Refresh `desc` only when it matches one we've installed
        // ourselves. A user-customized label (action managed but
        // display text edited) survives the seq migration.
        let existing_desc = b
            .as_table()
            .and_then(|t| t.get("desc"))
            .and_then(|v| v.as_str());
        let desc_is_managed = existing_desc.is_none_or(|d| managed_desc_set.contains(&d));
        let table = b.as_table_mut().unwrap();
        table.insert(
            "seq".into(),
            toml::Value::Array(
                current
                    .iter()
                    .map(|s| toml::Value::String((*s).into()))
                    .collect(),
            ),
        );
        if desc_is_managed {
            table.insert("desc".into(), toml::Value::String(desc.into()));
        }
    }
}

/// Rewrite the `desc` field of a binding when its desc matches a
/// historical form we shipped AND the action it points at still
/// runs one of our managed lua forms. The two checks together
/// keep us from touching user-owned bindings — neither a custom
/// desc on a managed action nor a managed desc on a user-owned
/// action will be rewritten. Same custom-desc preservation
/// rationale as [`migrate_seq_for_managed_binding`].
///
/// Idempotent: when the desc is already at index 0 of the history
/// (current form), no write happens.
fn migrate_binding_desc(
    bindings: &mut [toml::Value],
    actions: &[toml::Value],
    action_name: &str,
    current_desc: &str,
    desc_history: &[&str],
    lua_history: &[&str],
) {
    if desc_history.is_empty() {
        return;
    }
    // Look up the action's current lua body once. If it isn't in
    // our installed-history list (current form or any prior),
    // treat the action as user-owned and don't touch ANY of its
    // bindings' descs — even ones whose desc string happens to
    // match a historical form. The user owns the action; we don't
    // assume their desc is ours just because the strings line up.
    let action_lua = actions
        .iter()
        .find(|a| a.get("name").and_then(|n| n.as_str()) == Some(action_name))
        .and_then(|a| a.get("lua").and_then(|v| v.as_str()));
    let action_is_managed = match action_lua {
        Some(lua) => lua_history.contains(&lua),
        // No action with this name at all → no binding can refer
        // to it; bail without touching anything.
        None => return,
    };
    if !action_is_managed {
        return;
    }
    let prior: std::collections::HashSet<&str> = desc_history.iter().copied().collect();
    for b in bindings.iter_mut() {
        let Some(action) = b.get("action").and_then(|v| v.as_str()) else {
            continue;
        };
        if action != action_name {
            continue;
        }
        let Some(existing_desc) = b.get("desc").and_then(|v| v.as_str()) else {
            continue;
        };
        if existing_desc == current_desc {
            continue; // already current
        }
        if !prior.contains(existing_desc) {
            continue; // user-customized — leave alone
        }
        let table = b.as_table_mut().unwrap();
        table.insert("desc".into(), toml::Value::String(current_desc.into()));
    }
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
