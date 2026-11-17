use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;

use crate::error::{JjHooksError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateType {
    MoveForward,
    MoveBackward,
    MoveSideways,
    Add,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BookmarkUpdate {
    pub remote: String,
    pub bookmark: String,
    pub update_type: UpdateType,
    pub old_commit: Option<String>,
    pub new_commit: Option<String>,
}

impl std::fmt::Display for BookmarkUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let verb = match self.update_type {
            UpdateType::MoveForward => "Move forward",
            UpdateType::MoveBackward => "Move backward",
            UpdateType::MoveSideways => "Move sideways",
            UpdateType::Add => "Add",
            UpdateType::Delete => "Delete",
        };
        write!(f, "{verb} {}", self.bookmark)?;
        if let Some(old) = &self.old_commit {
            write!(f, " from {}", &old[..old.len().min(8)])?;
        }
        if let Some(new) = &self.new_commit {
            write!(f, " to {}", &new[..new.len().min(8)])?;
        }
        Ok(())
    }
}

struct PatternSet {
    update_type: UpdateType,
    patterns: Vec<Regex>,
}

fn patterns() -> &'static [PatternSet] {
    static PATTERNS: OnceLock<Vec<PatternSet>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            PatternSet {
                update_type: UpdateType::MoveForward,
                patterns: vec![
                    Regex::new(
                        r"Move forward bookmark (?P<bookmark>\S+) from (?P<old_commit>\w+) to (?P<new_commit>\w+)",
                    )
                    .unwrap(),
                    Regex::new(
                        r"^\s*bookmark:\s+(?P<bookmark>\S+)\s+\[move forward from (?P<old_commit>\w+) to (?P<new_commit>\w+)\]",
                    )
                    .unwrap(),
                ],
            },
            PatternSet {
                update_type: UpdateType::MoveBackward,
                patterns: vec![
                    Regex::new(
                        r"Move backward bookmark (?P<bookmark>\S+) from (?P<old_commit>\w+) to (?P<new_commit>\w+)",
                    )
                    .unwrap(),
                    Regex::new(
                        r"^\s*bookmark:\s+(?P<bookmark>\S+)\s+\[move backward from (?P<old_commit>\w+) to (?P<new_commit>\w+)\]",
                    )
                    .unwrap(),
                ],
            },
            PatternSet {
                update_type: UpdateType::MoveSideways,
                patterns: vec![
                    Regex::new(
                        r"Move sideways bookmark (?P<bookmark>\S+) from (?P<old_commit>\w+) to (?P<new_commit>\w+)",
                    )
                    .unwrap(),
                    Regex::new(
                        r"^\s*bookmark:\s+(?P<bookmark>\S+)\s+\[move sideways from (?P<old_commit>\w+) to (?P<new_commit>\w+)\]",
                    )
                    .unwrap(),
                ],
            },
            PatternSet {
                update_type: UpdateType::Add,
                patterns: vec![
                    Regex::new(r"Add bookmark (?P<bookmark>\S+) to (?P<new_commit>\w+)").unwrap(),
                    Regex::new(
                        r"^\s*bookmark:\s+(?P<bookmark>\S+)\s+\[add to (?P<new_commit>\w+)\]",
                    )
                    .unwrap(),
                ],
            },
            PatternSet {
                update_type: UpdateType::Delete,
                patterns: vec![
                    Regex::new(r"Delete bookmark (?P<bookmark>\S+) from (?P<old_commit>\w+)")
                        .unwrap(),
                    Regex::new(
                        r"^\s*bookmark:\s+(?P<bookmark>\S+)\s+\[delete from (?P<old_commit>\w+)\]",
                    )
                    .unwrap(),
                ],
            },
        ]
    })
}

fn remote_pattern() -> &'static Regex {
    static PAT: OnceLock<Regex> = OnceLock::new();
    PAT.get_or_init(|| Regex::new(r"^Changes to push to (.+?):").unwrap())
}

pub fn parse_git_push_dry_run(output: &str) -> Result<HashSet<BookmarkUpdate>> {
    let mut updates = HashSet::new();
    let mut remote: Option<String> = None;

    for line in output.lines() {
        if let Some(caps) = remote_pattern().captures(line) {
            remote = Some(caps.get(1).unwrap().as_str().to_owned());
            continue;
        }

        for set in patterns() {
            for pat in &set.patterns {
                let Some(caps) = pat.captures(line) else {
                    continue;
                };
                let Some(remote) = remote.as_ref() else {
                    return Err(JjHooksError::Parse(
                        "saw a bookmark line before any `Changes to push to <remote>:` line".into(),
                    ));
                };
                let bookmark = caps.name("bookmark").unwrap().as_str().to_owned();
                let old_commit = caps.name("old_commit").map(|m| m.as_str().to_owned());
                let new_commit = caps.name("new_commit").map(|m| m.as_str().to_owned());
                updates.insert(BookmarkUpdate {
                    remote: remote.clone(),
                    bookmark,
                    update_type: set.update_type,
                    old_commit,
                    new_commit,
                });
            }
        }
    }

    Ok(updates)
}
