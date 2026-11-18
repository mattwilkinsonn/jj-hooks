//! Shell completion support.
//!
//! `jj-hooks completions <shell>` emits a clap-generated completion script
//! for the user to source. Dynamic completers attached to specific flags
//! (e.g. `--bookmark`) shell out to `jj` to enumerate live values.
//!
//! All `jj` invocations here use `--ignore-working-copy` so completion
//! doesn't snapshot the user's working copy on every TAB.

use std::path::Path;
use std::process::Command;

use crate::error::Result;

/// Enumerate local bookmark names from the jj repo rooted at `cwd`. Returns
/// an empty list (not an error) when we're not inside a jj repo.
pub fn complete_bookmarks(cwd: &Path) -> Result<Vec<String>> {
    let out = Command::new("jj")
        .args([
            "bookmark",
            "list",
            "-T",
            "name ++ \"\\n\"",
            "--ignore-working-copy",
        ])
        .current_dir(cwd)
        .output()?;

    if !out.status.success() {
        // Not in a jj repo, or jj is broken — degrade silently.
        return Ok(vec![]);
    }

    Ok(parse_lines(&out.stdout))
}

/// Enumerate git remote names from the jj repo rooted at `cwd`. Output
/// format from `jj git remote list` is `<name> <url>` per line.
pub fn complete_remotes(cwd: &Path) -> Result<Vec<String>> {
    let out = Command::new("jj")
        .args(["git", "remote", "list", "--ignore-working-copy"])
        .current_dir(cwd)
        .output()?;

    if !out.status.success() {
        return Ok(vec![]);
    }

    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .filter_map(|line| line.split_whitespace().next().map(|s| s.to_owned()))
        .filter(|s| !s.is_empty())
        .collect())
}

fn parse_lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

/// `clap_complete`-compatible value completer for `--bookmark`. Reads the
/// current working directory at completion time.
pub fn bookmark_value_completer(
    current: &std::ffi::OsStr,
) -> Vec<clap_complete::CompletionCandidate> {
    let prefix = current.to_string_lossy();
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    complete_bookmarks(&cwd)
        .unwrap_or_default()
        .into_iter()
        .filter(|name| name.starts_with(prefix.as_ref()))
        .map(clap_complete::CompletionCandidate::new)
        .collect()
}

/// `clap_complete`-compatible value completer for `--remote`.
pub fn remote_value_completer(
    current: &std::ffi::OsStr,
) -> Vec<clap_complete::CompletionCandidate> {
    let prefix = current.to_string_lossy();
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    complete_remotes(&cwd)
        .unwrap_or_default()
        .into_iter()
        .filter(|name| name.starts_with(prefix.as_ref()))
        .map(clap_complete::CompletionCandidate::new)
        .collect()
}
