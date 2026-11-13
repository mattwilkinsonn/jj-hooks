use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum JjHooksError {
    #[error("jj exited with status {status}: {stderr}")]
    JjFailed { status: i32, stderr: String },

    /// A setup step (`[[jj-hooks.setup]]`) exited non-zero. The captured
    /// stdout+stderr of the failing step is attached so the caller can
    /// surface it — without this, the user sees only "setup step `bun`
    /// exited with status 1" and has nothing to debug.
    #[error("setup step `{name}` exited with status {status}")]
    SetupFailed {
        name: String,
        status: i32,
        captured: String,
    },
    #[error("gate did not run: waited {waited_secs}s for one of {slots} host-wide gate slots")]
    GateSlotTimeout { slots: usize, waited_secs: u64 },

    #[error("could not parse `jj git push --dry-run` output: {0}")]
    Parse(String),

    /// The configured runner binary couldn't be found through any of the
    /// resolution layers. Includes the binary name and a hint mentioning
    /// the most common causes (Python venv that wasn't activated, runner
    /// not installed globally, no `jj-hooks.runner-bin.<runner>` override).
    #[error(
        "hook runner `{bin}` could not be resolved.\n\
         jj-hp looked at: (1) `jj-hooks.runner-bin.{bin}` config, \
         (2) `.git/hooks/<stage>` shim from `prek install`, \
         (3) `uv run --` when `uv.lock` + `uv` are present (prek/pre-commit only), \
         (4) effective child PATH.\n\
         hint: install it globally (e.g. `brew install {bin}` / `pipx install {bin}` / `uv tool install {bin}`), \
         or set `jj-hooks.runner-bin.{bin} = \"…\"` in your jj config to point at the binary explicitly. \
         Effective PATH source: {path_source}."
    )]
    RunnerNotFound {
        bin: String,
        path_source: &'static str,
    },

    #[error(
        "hook runner `{bin}` was found at non-UTF-8 path `{path}` on {path_source}; use a UTF-8 path or an explicit wrapper"
    )]
    RunnerPathNotUtf8 {
        bin: String,
        path: String,
        path_source: &'static str,
    },

    #[error("could not spawn `{argv0}` in `{cwd}`: {source}")]
    Spawn {
        argv0: String,
        cwd: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A watched signal arrived while a hook worktree was live.
    #[error("interrupted by signal {signal}")]
    Interrupted { signal: i32 },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, JjHooksError>;

/// Preserve spawn context and the underlying operating-system error.
pub(crate) fn spawn_error(cmd: &Command, source: std::io::Error) -> JjHooksError {
    let cwd = cmd
        .get_current_dir()
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    JjHooksError::Spawn {
        argv0: cmd.get_program().to_string_lossy().into_owned(),
        cwd,
        source,
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_error_keeps_io_source_and_names_command_and_cwd() {
        let cwd = tempfile::TempDir::new().unwrap();
        let mut command = Command::new("missing-hook");
        command.current_dir(cwd.path());
        let source = std::io::Error::new(std::io::ErrorKind::NotFound, "no executable");
        let error = spawn_error(&command, source);

        assert!(matches!(
            &error,
            JjHooksError::Spawn { argv0, cwd: spawn_cwd, source }
                if argv0 == "missing-hook"
                    && spawn_cwd == cwd.path()
                    && source.kind() == std::io::ErrorKind::NotFound
        ));
        assert!(error.to_string().contains("missing-hook"));
        assert!(error.to_string().contains(cwd.path().to_str().unwrap()));
        assert_eq!(
            std::error::Error::source(&error)
                .map(ToString::to_string)
                .as_deref(),
            Some("no executable")
        );
    }
}
