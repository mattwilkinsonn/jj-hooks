//! Hook runner backends.
//!
//! Each runner has slightly different CLI ergonomics, so this module owns
//! the per-backend knowledge of "what args do I accept". pre-commit and
//! prek share a CLI shape; hk has its own; lefthook needs a file list
//! rather than ref bounds.
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::error::{JjHooksError, Result};
use crate::jj::JjCli;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    PreCommit,
    Prek,
    Lefthook,
    Hk,
}
const PREK_FILES: &[&str] = &["prek.toml", ".prek.toml"];

impl Runner {
    pub fn bin(self) -> &'static str {
        match self {
            Runner::PreCommit => "pre-commit",
            Runner::Prek => "prek",
            Runner::Lefthook => "lefthook",
            Runner::Hk => "hk",
        }
    }

    /// Filesystem probe for runner config files at `root`. Returns Ok(Some)
    /// for a single match, Ok(None) for no match, Err for ambiguous.
    pub fn autodetect(root: &Path) -> Result<Option<Runner>> {
        let candidates = Self::candidates();
        let mut found: Vec<Runner> = Vec::new();
        for (runner, files) in candidates {
            if files.iter().any(|f| root.join(f).exists()) {
                found.push(runner);
            }
        }

        // PreCommit + Prek aren't ambiguous — they're the same runner
        // family. prek consumes both `prek.toml` and `.pre-commit-config.yaml`,
        // so when both turn up at the same root, collapse to Prek rather
        // than asking the user to disambiguate.
        if found.contains(&Runner::Prek) && found.contains(&Runner::PreCommit) {
            found.retain(|r| *r != Runner::PreCommit);
        }

        match found.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(*one)),
            many => Err(crate::error::JjHooksError::Parse(format!(
                "multiple hook-runner configs found at workspace root: {:?}. Use --runner to pick one.",
                many.iter().map(|r| r.bin()).collect::<Vec<_>>()
            ))),
        }
    }

    fn candidates() -> [(Runner, &'static [&'static str]); 4] {
        [
            (Runner::Hk, &["hk.pkl"]),
            (
                Runner::Lefthook,
                &[
                    "lefthook.yml",
                    "lefthook.yaml",
                    "lefthook.json",
                    "lefthook.jsonc",
                    "lefthook.toml",
                    ".lefthook.yml",
                    ".lefthook.yaml",
                    ".lefthook.json",
                    ".lefthook.jsonc",
                    ".lefthook.toml",
                    ".config/lefthook.yml",
                    ".config/lefthook.yaml",
                    ".config/lefthook.json",
                    ".config/lefthook.jsonc",
                    ".config/lefthook.toml",
                ],
            ),
            (
                Runner::PreCommit,
                &[".pre-commit-config.yaml", ".pre-commit-config.yml"],
            ),
            (Runner::Prek, PREK_FILES),
        ]
    }

    pub fn config_path(self, root: &Path) -> Option<String> {
        let runner = if self == Runner::Prek && !PREK_FILES.iter().any(|p| root.join(p).exists()) {
            Runner::PreCommit
        } else {
            self
        };
        Self::candidates()
            .into_iter()
            .find(|(candidate, _)| *candidate == runner)
            .and_then(|(_, files)| files.iter().find(|file| root.join(file).exists()))
            .map(|path| (*path).to_owned())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    PreCommit,
    PrePush,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::PreCommit => "pre-commit",
            Stage::PrePush => "pre-push",
        }
    }
}

/// Build the argv for a hook invocation against the from..to ref range.
///
/// pre-commit / prek: `<bin> run --hook-stage <stage> --from-ref <from> --to-ref <to>`.
/// hk: `hk run <stage> --from-ref <from> --to-ref <to>` — hk takes the
/// same `--from-ref` / `--to-ref` flags as pre-commit, and *needs* them
/// when running in an ephemeral worktree (otherwise hk tries to resolve
/// `refs/remotes/origin/HEAD` and errors out).
///
/// Lefthook needs a file list, not refs — use [`lefthook_command`] instead.
pub fn hook_command(runner: Runner, stage: Stage, from: &str, to: &str) -> Vec<String> {
    match runner {
        Runner::PreCommit | Runner::Prek => vec![
            runner.bin().into(),
            "run".into(),
            "--hook-stage".into(),
            stage.as_str().into(),
            "--from-ref".into(),
            from.into(),
            "--to-ref".into(),
            to.into(),
        ],
        Runner::Hk => vec![
            runner.bin().into(),
            "run".into(),
            stage.as_str().into(),
            "--from-ref".into(),
            from.into(),
            "--to-ref".into(),
            to.into(),
        ],
        Runner::Lefthook => panic!(
            "lefthook does not take ref bounds; use lefthook_command with a file list instead"
        ),
    }
}

/// Build the argv for a lefthook invocation. Lefthook accepts repeated
/// `--file <path>` flags (one per changed file). When the file list is
/// empty we omit the flags entirely and let lefthook decide whether
/// "nothing to do" is a success or no-op.
pub fn lefthook_command(stage: Stage, files: &[PathBuf]) -> Vec<String> {
    let mut argv = vec!["lefthook".into(), "run".into(), stage.as_str().into()];
    for f in files {
        argv.push("--file".into());
        argv.push(f.to_string_lossy().into_owned());
    }
    argv
}

/// Build the argv for a runner invocation in `--all-files` mode. The
/// runner's own "ignore the diff, lint every tracked file" flag replaces
/// the `--from-ref`/`--to-ref` selection [`hook_command`] would normally
/// pass.
///
/// Per-runner mapping (verified against each tool):
///   pre-commit / prek: `--all-files`
///   hk:                `--glob '*'` (hk's `-a/--all` does NOT override
///                      its from/to-ref defaults on stage hooks, despite
///                      what `hk run --help` implies; `--glob '*'` is the
///                      only flag that actually replaces the file
///                      selection. Verified with hk 1.45.0.)
///
/// Lefthook is symmetric to [`hook_command`] — it needs its own builder
/// (`lefthook_command_all_files`) because the all-files form replaces
/// the per-file selection rather than the ref bounds.
pub fn hook_command_all_files(runner: Runner, stage: Stage) -> Vec<String> {
    match runner {
        Runner::PreCommit | Runner::Prek => vec![
            runner.bin().into(),
            "run".into(),
            "--hook-stage".into(),
            stage.as_str().into(),
            "--all-files".into(),
        ],
        Runner::Hk => vec![
            runner.bin().into(),
            "run".into(),
            stage.as_str().into(),
            "--glob".into(),
            "*".into(),
        ],
        Runner::Lefthook => {
            panic!("lefthook is built via lefthook_command_all_files, not hook_command_all_files")
        }
    }
}

/// Build the argv that warms hk's Pkl cache: `hk validate` evaluates
/// `hk.pkl` (resolving + caching its `package://` imports) without
/// running any hook or needing git refs. The bare `hk` element is
/// spliced with the resolved runner prefix by the caller, the same way
/// [`hook_command`] is.
pub fn hk_validate_command() -> Vec<String> {
    vec![Runner::Hk.bin().into(), "validate".into()]
}

/// Build the argv for a lefthook invocation in all-files mode.
/// Lefthook's `--all-files` flag replaces the per-`--file` selection
/// [`lefthook_command`] would otherwise build.
pub fn lefthook_command_all_files(stage: Stage) -> Vec<String> {
    vec![
        "lefthook".into(),
        "run".into(),
        stage.as_str().into(),
        "--all-files".into(),
    ]
}

/// Swap `Runner::PreCommit` for `Runner::Prek` when prek is on the user's
/// PATH. prek is a drop-in pre-commit replacement that's much faster, so
/// users who happen to have both installed should get the faster one
/// automatically. An explicit `--runner pre-commit` short-circuits this
/// (callers should only invoke `prefer_prek_when_available` on the
/// autodetected result, not on a user-supplied override).
pub fn prefer_prek_when_available(autodetected: Runner, prek_present: bool) -> Runner {
    match (autodetected, prek_present) {
        (Runner::PreCommit, true) => Runner::Prek,
        _ => autodetected,
    }
}

/// Probe `$PATH` for the `prek` binary. Used by [`prefer_prek_when_available`]
/// in test setups; production code uses [`resolve_runner_argv`] which
/// covers the wider set of layers.
pub fn prek_on_path() -> bool {
    which("prek").is_some()
}

fn which(bin: &str) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    which_in_path(bin, std::env::var_os("PATH").as_deref(), &cwd)
}

fn which_in_path(bin: &str, path: Option<&OsStr>, cwd: &Path) -> Option<PathBuf> {
    let path = path?;
    let cwd = std::path::absolute(cwd).ok()?;
    for dir in std::env::split_paths(path) {
        let candidate = if dir.is_absolute() {
            dir.join(bin)
        } else {
            cwd.join(dir).join(bin)
        };
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn resolve_on_path(
    bin: &str,
    path: Option<&OsStr>,
    cwd: &Path,
    path_source: &'static str,
) -> Result<Vec<String>> {
    if let Some(path) = which_in_path(bin, path, cwd) {
        return Ok(vec![path.to_string_lossy().into_owned()]);
    }

    Err(JjHooksError::RunnerNotFound {
        bin: bin.to_owned(),
        path_source,
    })
}
/// Resolve a hook runner to an argv prefix.
///
/// The prefix replaces the bare runner name when building the hook command.
/// Resolution order is explicit config, an installed shim, a `uv`-managed
/// environment for Python runners, then the patched or inherited child PATH.
/// Relative PATH entries resolve against the child working directory.
///
/// # Errors
///
/// Returns [`JjHooksError::RunnerNotFound`] when no layer finds the runner.
/// The error includes the effective PATH source used for lookup.
pub fn resolve_runner_argv(
    runner: Runner,
    jj: &JjCli,
    workspace_root: &Path,
    primary_git_dir: &Path,
    stage: Stage,
) -> Result<Vec<String>> {
    resolve_runner_argv_for_cwd(runner, jj, workspace_root, primary_git_dir, stage, jj.cwd())
}

pub(crate) fn resolve_runner_argv_for_cwd(
    runner: Runner,
    jj: &JjCli,
    workspace_root: &Path,
    primary_git_dir: &Path,
    stage: Stage,
    child_cwd: &Path,
) -> Result<Vec<String>> {
    // (1) Explicit config override.
    if let Some(argv) = read_runner_bin_config(jj, runner, workspace_root)? {
        tracing::debug!("runner {}: resolved via config: {argv:?}", runner.bin());
        return Ok(argv);
    }

    // (2) Hook-shim path. Both `prek install` and `pre-commit install`
    // bake the resolved binary into `.git/hooks/<stage>`:
    //
    // - prek writes `PREK="/abs/path/to/.venv/bin/prek"` and `exec`s
    //   it directly.
    // - pre-commit writes `INSTALL_PYTHON=/abs/path/to/.venv/bin/python`
    //   and `exec`s it as `"$INSTALL_PYTHON" -mpre_commit "${ARGS[@]}"` —
    //   `INSTALL_PYTHON` is the interpreter, not pre-commit itself, but
    //   `python -m pre_commit` is still pre-commit's entry point.
    //
    // For prek the resolved argv is a single element (the path);
    // for pre-commit it's `[python, "-mpre_commit"]`.
    if let Some(argv) = read_shim_argv(primary_git_dir, stage, runner) {
        tracing::debug!(
            "runner {}: resolved via .git/hooks/{} shim: {argv:?}",
            runner.bin(),
            stage.as_str(),
        );
        return Ok(argv);
    }

    // Resolve PATH once from the same repo-env patch that will be applied
    // to the child command. A patched or removed PATH must not be replaced
    // by the parent process's PATH during this lookup.
    let (effective_path, path_source) = crate::repo_env::effective_path(workspace_root);

    // (3) uv-managed venv. Only for pre-commit / prek (lefthook and
    // hk aren't Python tools). Requires both uv.lock in the workspace
    // and `uv` itself on the effective child PATH.
    // We pass `--project <workspace_root>` so uv resolves the venv
    // relative to the user's actual workspace, not the ephemeral
    // worktree we run hooks in. The worktree is a fresh git checkout
    // and (typically) doesn't have `.venv` since `.venv` is gitignored
    // — without --project, uv would either fail to find the runner
    // or try to bootstrap a fresh env every push.
    if matches!(runner, Runner::PreCommit | Runner::Prek)
        && workspace_root.join("uv.lock").exists()
        && let Some(uv) = which_in_path("uv", effective_path.as_deref(), child_cwd)
    {
        tracing::debug!("runner {}: resolved via `uv run --project`", runner.bin());
        return Ok(vec![
            uv.to_string_lossy().into_owned(),
            "run".into(),
            "--project".into(),
            workspace_root.to_string_lossy().into_owned(),
            "--".into(),
            runner.bin().into(),
        ]);
    }

    // (4) The effective child PATH. Resolve the matching absolute path so
    // exec does not search the parent PATH or resolve relative entries elsewhere.
    resolve_on_path(
        runner.bin(),
        effective_path.as_deref(),
        child_cwd,
        path_source,
    )
}

/// Read `jj-hooks.runner-bin.<runner>` from jj config and return it as an
/// argv prefix. Accepts:
///
/// - bare string: `runner-bin.prek = ".venv/bin/prek"`
///   → `[".venv/bin/prek"]` (or absolute equivalent against `workspace_root`)
/// - array of strings: `runner-bin.prek = ["uv", "run", "--", "prek"]`
///   → returned as-is
///
/// Returns `Ok(None)` when the key isn't set. Errors when the value is
/// present but malformed (empty array, non-string element, etc.) so the
/// user catches typos at config-load time rather than seeing the wrong
/// binary silently invoked.
fn read_runner_bin_config(
    jj: &JjCli,
    runner: Runner,
    workspace_root: &Path,
) -> Result<Option<Vec<String>>> {
    let key = format!("jj-hooks.runner-bin.{}", runner.bin());
    let Ok(raw) = jj.run(&["config", "get", &key]) else {
        // Key missing — `jj config get` exits non-zero. That's the
        // common no-override path, not an error.
        return Ok(None);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }

    let argv =
        parse_runner_bin_value(raw).map_err(|e| JjHooksError::Parse(format!("{key}: {e}")))?;

    // First element gets resolved against workspace_root when relative.
    // Subsequent elements are plain args (`uv run --`, etc.) and pass
    // through verbatim — they're not paths.
    let mut out = argv;
    if let Some(first) = out.first_mut() {
        let p = Path::new(first);
        if p.is_relative() {
            *first = workspace_root.join(p).to_string_lossy().into_owned();
        }
    }
    Ok(Some(out))
}

/// Parse a `jj config get jj-hooks.runner-bin.<runner>` value into argv.
/// Accepts either a bare string (the form `jj config get` uses for
/// scalar values — unquoted, raw) or a TOML inline array (the form
/// jj uses for array values, e.g. `["uv", "run", "--", "prek"]`).
/// Empty arrays and non-string array elements are rejected.
fn parse_runner_bin_value(raw: &str) -> std::result::Result<Vec<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("must not be empty".into());
    }
    if trimmed.starts_with('[') {
        // Array form. Use the standard TOML deserializer.
        let wrapped = format!("v = {trimmed}");
        #[derive(serde::Deserialize)]
        struct Wrap {
            v: Vec<String>,
        }
        let parsed: Wrap = toml::from_str(&wrapped).map_err(|e| {
            format!("array form must be all strings (e.g. [\"uv\", \"run\", \"--\", \"prek\"]); got {raw:?}: {e}")
        })?;
        if parsed.v.is_empty() {
            return Err("array must have at least one element".into());
        }
        if parsed.v.iter().any(String::is_empty) {
            return Err("array elements must be non-empty strings".into());
        }
        return Ok(parsed.v);
    }

    // Scalar form. `jj config get` prints scalar values raw (no
    // surrounding quotes), so we take the trimmed string verbatim.
    // Strip surrounding quotes if present (in case the user copies
    // the value out of a TOML file and pastes it as-is).
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(trimmed);
    Ok(vec![unquoted.to_owned()])
}

/// Parse `primary_git_dir/hooks/<stage>` for the runner path baked in by
/// `prek install` or `pre-commit install`. Returns the argv prefix that
/// should be used to invoke the runner.
///
/// Shim formats (stable across recent versions of each tool):
///
/// prek:
/// ```sh
/// PREK="/abs/path/to/.venv/bin/prek"
/// exec "$PREK" hook-impl …
/// ```
///
/// pre-commit:
/// ```sh
/// INSTALL_PYTHON=/abs/path/to/.venv/bin/python
/// ARGS=(hook-impl …)
/// exec "$INSTALL_PYTHON" -mpre_commit "${ARGS[@]}"
/// ```
///
/// For prek the returned argv is `[PREK]`; for pre-commit it's
/// `[INSTALL_PYTHON, "-mpre_commit"]`. The caller splices the runner's
/// subcommand args (`run --hook-stage …`) on after.
///
/// We don't try to be a full shell parser — the install scripts always
/// emit a simple assignment on its own line. prek quotes the value,
/// pre-commit does not; both are handled.
///
/// Returns `None` for runners that don't have a recognised shim format
/// (lefthook, hk) or when the shim is missing / unrecognised / points
/// at a non-existent path.
fn read_shim_argv(primary_git_dir: &Path, stage: Stage, runner: Runner) -> Option<Vec<String>> {
    let (var_name, build_argv): (&str, fn(PathBuf) -> Vec<String>) = match runner {
        Runner::Prek => ("PREK", |p| vec![p.to_string_lossy().into_owned()]),
        Runner::PreCommit => ("INSTALL_PYTHON", |p| {
            vec![p.to_string_lossy().into_owned(), "-mpre_commit".into()]
        }),
        // hk and lefthook install their own shim formats; we don't try
        // to parse those.
        Runner::Hk | Runner::Lefthook => return None,
    };

    let shim = primary_git_dir.join("hooks").join(stage.as_str());
    let body = std::fs::read_to_string(&shim).ok()?;
    for line in body.lines() {
        let trimmed = line.trim();
        // Tolerate a leading `export ` (some shim variants emit it).
        let after_export = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some(rest) = after_export.strip_prefix(var_name) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix('=') else {
            // Avoid matching `PREKABLE=…` against `PREK`.
            continue;
        };
        // Strip surrounding double quotes if present (prek quotes,
        // pre-commit doesn't).
        let path_str = rest
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(rest);
        let candidate = PathBuf::from(path_str);
        // Only accept absolute paths — relative paths in a shim
        // would resolve against $PWD at hook-invocation time, which
        // is not what we want here. (prek's `if [ ! -x "$PREK" ]`
        // fallback writes `PREK="prek"`; that bare name is intentionally
        // ignored — we continue to subsequent resolution layers.)
        if !candidate.is_absolute() {
            continue;
        }
        if candidate.is_file() {
            return Some(build_argv(candidate));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_executable(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn resolve_on_path_finds_runner_only_in_patched_path() {
        let parent_bin = tempfile::TempDir::new().unwrap();
        let patched_bin = tempfile::TempDir::new().unwrap();
        write_executable(parent_bin.path(), Runner::Hk.bin());
        write_executable(patched_bin.path(), Runner::Lefthook.bin());

        assert_eq!(
            resolve_on_path(
                Runner::Lefthook.bin(),
                Some(patched_bin.path().as_os_str()),
                patched_bin.path(),
                "repo-env PATH patch",
            )
            .unwrap(),
            vec![
                patched_bin
                    .path()
                    .join(Runner::Lefthook.bin())
                    .canonicalize()
                    .unwrap()
                    .display()
                    .to_string()
            ]
        );
        let error = resolve_on_path(
            Runner::Hk.bin(),
            Some(patched_bin.path().as_os_str()),
            patched_bin.path(),
            "repo-env PATH patch",
        )
        .unwrap_err();
        assert!(
            matches!(error, JjHooksError::RunnerNotFound { ref bin, path_source: "repo-env PATH patch" } if bin == Runner::Hk.bin())
        );
    }

    #[test]
    fn resolve_on_path_does_not_fall_back_when_path_is_removed() {
        let parent_bin = tempfile::TempDir::new().unwrap();
        write_executable(parent_bin.path(), Runner::Hk.bin());
        let error = resolve_on_path(
            Runner::Hk.bin(),
            None,
            parent_bin.path(),
            "PATH removed by repo-env patch",
        )
        .unwrap_err();
        assert!(
            matches!(error, JjHooksError::RunnerNotFound { ref bin, path_source: "PATH removed by repo-env patch" } if bin == Runner::Hk.bin())
        );
    }

    #[test]
    fn resolve_on_path_uses_inherited_path_for_disabled_patch() {
        let inherited_bin = tempfile::TempDir::new().unwrap();
        let expected = write_executable(inherited_bin.path(), Runner::Hk.bin());
        let result = resolve_on_path(
            Runner::Hk.bin(),
            Some(inherited_bin.path().as_os_str()),
            inherited_bin.path(),
            "inherited PATH",
        )
        .unwrap();
        assert_eq!(
            result,
            vec![
                inherited_bin
                    .path()
                    .join(Runner::Hk.bin())
                    .canonicalize()
                    .unwrap()
                    .display()
                    .to_string()
            ]
        );
        assert!(expected.is_file());
    }

    #[test]
    fn uv_lookup_uses_the_effective_path() {
        let parent_bin = tempfile::TempDir::new().unwrap();
        let patched_bin = tempfile::TempDir::new().unwrap();
        write_executable(parent_bin.path(), "uv");
        assert!(
            which_in_path(
                "uv",
                Some(patched_bin.path().as_os_str()),
                patched_bin.path(),
            )
            .is_none()
        );
        let patched_uv = write_executable(patched_bin.path(), "uv");
        assert_eq!(
            which_in_path(
                "uv",
                Some(patched_bin.path().as_os_str()),
                patched_bin.path(),
            ),
            Some(patched_uv.canonicalize().unwrap())
        );
    }

    #[test]
    fn resolve_on_path_uses_seeded_repo_env_path_semantics() {
        let workspace = tempfile::TempDir::new().unwrap();
        let parent_bin = tempfile::TempDir::new().unwrap();
        let patched_bin = tempfile::TempDir::new().unwrap();
        write_executable(parent_bin.path(), Runner::Hk.bin());
        write_executable(patched_bin.path(), Runner::Lefthook.bin());

        crate::repo_env::test_seed(
            workspace.path(),
            crate::repo_env::EnvPatch::Patch(std::collections::HashMap::from([(
                "PATH".to_owned(),
                Some(patched_bin.path().display().to_string()),
            )])),
        );
        let (path, source) = crate::repo_env::effective_path_with_parent(
            workspace.path(),
            Some(parent_bin.path().as_os_str()),
        );
        assert_eq!(source, "repo-env PATH patch");
        assert_eq!(
            resolve_on_path(
                Runner::Lefthook.bin(),
                path.as_deref(),
                workspace.path(),
                source,
            )
            .unwrap(),
            vec![
                patched_bin
                    .path()
                    .join(Runner::Lefthook.bin())
                    .canonicalize()
                    .unwrap()
                    .display()
                    .to_string()
            ]
        );
        assert!(matches!(
            resolve_on_path(Runner::Hk.bin(), path.as_deref(), workspace.path(), source),
            Err(JjHooksError::RunnerNotFound { ref bin, .. }) if bin == Runner::Hk.bin()
        ));

        crate::repo_env::test_seed(
            workspace.path(),
            crate::repo_env::EnvPatch::Patch(std::collections::HashMap::from([(
                "PATH".to_owned(),
                None,
            )])),
        );
        let (path, source) = crate::repo_env::effective_path_with_parent(
            workspace.path(),
            Some(parent_bin.path().as_os_str()),
        );
        assert!(path.is_none());
        assert!(matches!(
            resolve_on_path(Runner::Hk.bin(), path.as_deref(), workspace.path(), source),
            Err(JjHooksError::RunnerNotFound { ref bin, path_source: "PATH removed by repo-env patch" }) if bin == Runner::Hk.bin()
        ));

        crate::repo_env::test_seed(workspace.path(), crate::repo_env::EnvPatch::Disabled);
        let (path, source) = crate::repo_env::effective_path_with_parent(
            workspace.path(),
            Some(parent_bin.path().as_os_str()),
        );
        assert_eq!(source, "inherited PATH");
        assert_eq!(
            resolve_on_path(Runner::Hk.bin(), path.as_deref(), workspace.path(), source).unwrap(),
            vec![
                parent_bin
                    .path()
                    .join(Runner::Hk.bin())
                    .canonicalize()
                    .unwrap()
                    .display()
                    .to_string()
            ]
        );
    }

    #[test]
    #[cfg(unix)]
    fn relative_path_entries_resolve_from_child_cwd() {
        let parent_cwd = tempfile::TempDir::new().unwrap();
        let child_cwd = tempfile::TempDir::new().unwrap();
        let parent_bin = parent_cwd.path().join("bin");
        let child_bin = child_cwd.path().join("bin");
        std::fs::create_dir(&parent_bin).unwrap();
        std::fs::create_dir(&child_bin).unwrap();
        write_executable(&parent_bin, Runner::Hk.bin());
        write_executable(&child_bin, Runner::Hk.bin());

        let parent_found =
            which_in_path(Runner::Hk.bin(), Some(OsStr::new("bin")), parent_cwd.path());
        let child_found =
            which_in_path(Runner::Hk.bin(), Some(OsStr::new("bin")), child_cwd.path());

        assert_eq!(parent_found, Some(parent_bin.join(Runner::Hk.bin())));
        assert_eq!(child_found, Some(child_bin.join(Runner::Hk.bin())));
        assert_ne!(parent_found, child_found);
    }

    #[test]
    #[cfg(unix)]
    fn empty_path_entry_resolves_from_child_cwd() {
        let child_cwd = tempfile::TempDir::new().unwrap();
        write_executable(child_cwd.path(), Runner::Hk.bin());

        let found = which_in_path(Runner::Hk.bin(), Some(OsStr::new("")), child_cwd.path());
        assert_eq!(found, Some(child_cwd.path().join(Runner::Hk.bin())));
    }

    #[test]
    #[cfg(unix)]
    fn earlier_non_executable_file_is_skipped_for_later_binary() {
        use std::os::unix::fs::PermissionsExt;

        let cwd = tempfile::TempDir::new().unwrap();
        let first = tempfile::TempDir::new().unwrap();
        let second = tempfile::TempDir::new().unwrap();
        let blocked = first.path().join(Runner::Hk.bin());
        std::fs::write(&blocked, b"not executable").unwrap();
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o644)).unwrap();
        let expected = write_executable(second.path(), Runner::Hk.bin());

        let path = std::env::join_paths([first.path(), second.path()]).unwrap();
        let resolved = resolve_on_path(Runner::Hk.bin(), Some(&path), cwd.path(), "inherited PATH")
            .unwrap();
        assert_eq!(resolved, vec![expected.display().to_string()]);
    }
    // -- parse_runner_bin_value --------------------------------------------

    #[test]
    fn parse_runner_bin_value_bare_unquoted_string() {
        // `jj config get jj-hooks.runner-bin.prek` for a scalar value
        // prints the string unquoted. That's the primary form we see
        // in practice.
        let argv = parse_runner_bin_value("prek").unwrap();
        assert_eq!(argv, vec!["prek"]);
    }

    #[test]
    fn parse_runner_bin_value_absolute_path() {
        let argv = parse_runner_bin_value("/abs/path/to/prek").unwrap();
        assert_eq!(argv, vec!["/abs/path/to/prek"]);
    }

    #[test]
    fn parse_runner_bin_value_quoted_string_strips_quotes() {
        // Defensive: if the user copies the TOML quoted form, accept
        // it rather than including the literal quotes in argv[0].
        let argv = parse_runner_bin_value(r#""/abs/path/to/prek""#).unwrap();
        assert_eq!(argv, vec!["/abs/path/to/prek"]);
    }

    #[test]
    fn parse_runner_bin_value_array() {
        // The shape printed for `runner-bin.prek = ["uv", "run", "--", "prek"]`.
        let argv = parse_runner_bin_value(r#"["uv", "run", "--", "prek"]"#).unwrap();
        assert_eq!(argv, vec!["uv", "run", "--", "prek"]);
    }

    #[test]
    fn parse_runner_bin_value_empty_string_errors() {
        // Defensive: an empty string config value is almost certainly a
        // user typo. Reject so they catch it now rather than seeing the
        // resolver fall through to PATH and pick up the wrong binary.
        let err = parse_runner_bin_value("").unwrap_err();
        assert!(err.contains("empty"), "expected empty-string error: {err}");
    }

    #[test]
    fn parse_runner_bin_value_empty_array_errors() {
        let err = parse_runner_bin_value("[]").unwrap_err();
        assert!(err.contains("at least one"), "got: {err}");
    }

    #[test]
    fn parse_runner_bin_value_array_with_empty_element_errors() {
        let err = parse_runner_bin_value(r#"["uv", ""]"#).unwrap_err();
        assert!(err.contains("non-empty"), "got: {err}");
    }

    #[test]
    fn parse_runner_bin_value_non_string_array_errors() {
        // A number masquerading as a binary path is a typo we should
        // catch loudly, not silently coerce.
        let err = parse_runner_bin_value(r#"["uv", 42]"#).unwrap_err();
        assert!(
            err.contains("string"),
            "expected string-related error: {err}"
        );
    }

    // -- read_shim_argv ----------------------------------------------------

    /// Build a temp `<dir>/hooks/<stage>` shim file with given contents
    /// and return its parent (the simulated git dir) for `read_shim_argv`.
    fn write_shim(stage: Stage, body: &str) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let hooks = dir.path().join("hooks");
        std::fs::create_dir(&hooks).unwrap();
        std::fs::write(hooks.join(stage.as_str()), body).unwrap();
        dir
    }

    #[test]
    fn read_shim_argv_returns_none_when_shim_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(
            read_shim_argv(dir.path(), Stage::PreCommit, Runner::Prek),
            None
        );
    }

    #[test]
    fn read_shim_argv_returns_none_when_shim_unrecognised() {
        // A shim with no recognised assignment — should silently fall
        // through to layer 3/4 rather than erroring.
        let dir = write_shim(Stage::PreCommit, "#!/bin/sh\nexec prek hook-impl\n");
        assert_eq!(
            read_shim_argv(dir.path(), Stage::PreCommit, Runner::Prek),
            None
        );
    }

    #[test]
    #[cfg(unix)] // resolves /bin/sh as an executable; Windows has no such path
    fn read_shim_argv_picks_up_prek_install_format() {
        // The exact format `prek install` writes (issue #17 repro).
        // We need the path to resolve to a real executable for the
        // gate to fire — point at /bin/sh which exists on every Unix.
        let body = r#"#!/bin/sh
HERE="$(cd "$(dirname "$0")" && pwd)"
PREK="/bin/sh"
if [ ! -x "$PREK" ]; then
    PREK="prek"
fi
exec "$PREK" hook-impl --hook-dir "$HERE" --script-version 4 --hook-type=pre-commit -- "$@"
"#;
        let dir = write_shim(Stage::PreCommit, body);
        let argv = read_shim_argv(dir.path(), Stage::PreCommit, Runner::Prek);
        assert_eq!(argv, Some(vec!["/bin/sh".to_owned()]));
    }

    #[test]
    #[cfg(unix)] // resolves /bin/sh as an executable; Windows has no such path
    fn read_shim_argv_picks_up_pre_commit_install_format() {
        // The format `pre-commit install` writes: unquoted
        // INSTALL_PYTHON=<path>, then exec'd as `python -mpre_commit`.
        // The resolved argv must include the `-mpre_commit` flag —
        // running `INSTALL_PYTHON` bare would just give you a Python
        // REPL, not pre-commit.
        let body = r#"#!/usr/bin/env bash
# start templated
INSTALL_PYTHON=/bin/sh
ARGS=(hook-impl --config=.pre-commit-config.yaml --hook-type=pre-commit)
# end templated
HERE="$(cd "$(dirname "$0")" && pwd)"
ARGS+=(--hook-dir "$HERE" -- "$@")
exec "$INSTALL_PYTHON" -mpre_commit "${ARGS[@]}"
"#;
        let dir = write_shim(Stage::PreCommit, body);
        let argv = read_shim_argv(dir.path(), Stage::PreCommit, Runner::PreCommit);
        assert_eq!(
            argv,
            Some(vec!["/bin/sh".to_owned(), "-mpre_commit".to_owned()])
        );
    }

    #[test]
    fn read_shim_argv_runner_specific_var_name() {
        // A prek-format shim shouldn't satisfy the pre-commit probe,
        // and vice versa — each runner has its own baked variable.
        let prek_body = r#"PREK="/bin/sh""#;
        let dir = write_shim(Stage::PreCommit, prek_body);
        assert_eq!(
            read_shim_argv(dir.path(), Stage::PreCommit, Runner::PreCommit),
            None,
            "PREK= line must not satisfy the pre-commit shim probe"
        );

        let pc_body = "INSTALL_PYTHON=/bin/sh";
        let dir = write_shim(Stage::PreCommit, pc_body);
        assert_eq!(
            read_shim_argv(dir.path(), Stage::PreCommit, Runner::Prek),
            None,
            "INSTALL_PYTHON= line must not satisfy the prek shim probe"
        );
    }

    #[test]
    fn read_shim_argv_skips_assignments_pointing_at_nonexistent_path() {
        // The shim's PREK var points at a venv that no longer exists
        // (user deleted .venv but `prek uninstall` was never run).
        // We should skip and fall through to subsequent layers, not
        // resolve to a dead path that would later fail on spawn.
        let body = r#"PREK="/nonexistent/path/to/prek""#;
        let dir = write_shim(Stage::PreCommit, body);
        assert_eq!(
            read_shim_argv(dir.path(), Stage::PreCommit, Runner::Prek),
            None
        );
    }

    #[test]
    fn read_shim_argv_skips_relative_paths() {
        // A relative path in a shim would resolve against $PWD at
        // hook-invocation time. That's never what we want here — only
        // accept absolute paths.
        let body = r#"PREK="prek""#;
        let dir = write_shim(Stage::PreCommit, body);
        assert_eq!(
            read_shim_argv(dir.path(), Stage::PreCommit, Runner::Prek),
            None
        );
    }

    #[test]
    #[cfg(unix)] // resolves /bin/sh as an executable; Windows has no such path
    fn read_shim_argv_honours_stage() {
        // The pre-commit shim must NOT be consulted when we're running
        // the pre-push stage (and vice versa) — each stage has its own
        // installed shim with potentially different baked-in paths.
        let body = r#"PREK="/bin/sh""#;
        let dir = write_shim(Stage::PreCommit, body);
        assert_eq!(
            read_shim_argv(dir.path(), Stage::PreCommit, Runner::Prek),
            Some(vec!["/bin/sh".to_owned()])
        );
        assert_eq!(
            read_shim_argv(dir.path(), Stage::PrePush, Runner::Prek),
            None
        );
    }

    #[test]
    #[cfg(unix)] // resolves /bin/sh as an executable; Windows has no such path
    fn read_shim_argv_accepts_export_prefix() {
        // Some shim variants emit `export VAR="…"` rather than bare
        // `VAR="…"`. Tolerate that.
        let body = r#"export PREK="/bin/sh""#;
        let dir = write_shim(Stage::PreCommit, body);
        assert_eq!(
            read_shim_argv(dir.path(), Stage::PreCommit, Runner::Prek),
            Some(vec!["/bin/sh".to_owned()])
        );
    }

    #[test]
    fn read_shim_argv_only_matches_exact_variable_name() {
        // Defensive: `PREKABLE=…` shouldn't match `PREK=`, and
        // `INSTALL_PYTHON_VERSION=…` shouldn't match `INSTALL_PYTHON=`.
        let body = r#"PREKABLE="/bin/sh""#;
        let dir = write_shim(Stage::PreCommit, body);
        assert_eq!(
            read_shim_argv(dir.path(), Stage::PreCommit, Runner::Prek),
            None
        );
    }

    #[test]
    fn read_shim_argv_returns_none_for_lefthook_and_hk() {
        // We don't try to parse lefthook / hk install shims — their
        // formats are different and harder to dispatch on. These
        // runners are only resolved via layers 1 / 4.
        let body = r#"PREK="/bin/sh""#;
        let dir = write_shim(Stage::PreCommit, body);
        assert_eq!(
            read_shim_argv(dir.path(), Stage::PreCommit, Runner::Lefthook),
            None
        );
        assert_eq!(
            read_shim_argv(dir.path(), Stage::PreCommit, Runner::Hk),
            None
        );
    }

    #[test]
    fn hk_validate_command_is_hk_validate() {
        assert_eq!(
            hk_validate_command(),
            vec!["hk".to_string(), "validate".to_string()]
        );
    }
}
