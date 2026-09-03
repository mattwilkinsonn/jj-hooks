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
         (4) `$PATH`.\n\
         hint: install it globally (e.g. `brew install {bin}` / `pipx install {bin}` / `uv tool install {bin}`), \
         or set `jj-hooks.runner-bin.{bin} = \"…\"` in your jj config to point at the binary explicitly."
    )]
    RunnerNotFound { bin: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, JjHooksError>;
