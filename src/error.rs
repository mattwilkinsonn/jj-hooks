use thiserror::Error;

#[derive(Debug, Error)]
pub enum JjHooksError {
    #[error("jj exited with status {status}: {stderr}")]
    JjFailed { status: i32, stderr: String },

    #[error("could not parse `jj git push --dry-run` output: {0}")]
    Parse(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, JjHooksError>;
