//! Tests for the bookmark + remote completers.

mod harness;

use harness::{TestRepo, show};
use jj_hooks::completions::{complete_bookmarks, complete_remotes};

#[test]
fn complete_bookmarks_lists_local_bookmarks() {
    let repo = TestRepo::new();
    // Harness already creates `main`. Add another for completeness.
    let out = repo.jj(&["bookmark", "create", "feature", "-r", "@-"]);
    assert!(out.status.success(), "{}", show(&out));

    let names = complete_bookmarks(repo.primary()).unwrap();
    assert!(names.contains(&"main".to_string()), "{names:?}");
    assert!(names.contains(&"feature".to_string()), "{names:?}");
}

#[test]
fn complete_bookmarks_outside_a_repo_returns_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let names = complete_bookmarks(tmp.path()).unwrap_or_default();
    assert!(names.is_empty(), "{names:?}");
}

#[test]
fn complete_remotes_lists_origin() {
    let repo = TestRepo::new();
    let names = complete_remotes(repo.primary()).unwrap();
    assert!(names.contains(&"origin".to_string()), "{names:?}");
}
