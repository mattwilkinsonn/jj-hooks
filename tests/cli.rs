//! Tests for reconstructing `jj git push` argv from our parsed `Push` flags.

use jj_hooks::cli::{PushArgs, push_argv};

fn empty() -> PushArgs {
    PushArgs {
        bookmark: vec![],
        revision: vec![],
        change: vec![],
        remote: None,
        all: false,
        tracked: false,
        deleted: false,
        passthrough: vec![],
    }
}

#[test]
fn empty_args_no_flags() {
    let argv = push_argv(&empty(), false);
    assert!(argv.is_empty(), "{argv:?}");
}

#[test]
fn dry_run_emits_flag() {
    let argv = push_argv(&empty(), true);
    assert_eq!(argv, vec!["--dry-run"]);
}

#[test]
fn single_bookmark() {
    let args = PushArgs {
        bookmark: vec!["main".into()],
        ..empty()
    };
    assert_eq!(push_argv(&args, false), vec!["-b", "main"]);
}

#[test]
fn multiple_bookmarks_each_get_dash_b() {
    let args = PushArgs {
        bookmark: vec!["main".into(), "feature".into()],
        ..empty()
    };
    assert_eq!(push_argv(&args, false), vec!["-b", "main", "-b", "feature"]);
}

#[test]
fn revision_and_change() {
    let args = PushArgs {
        revision: vec!["@".into()],
        change: vec!["abc".into(), "def".into()],
        ..empty()
    };
    assert_eq!(
        push_argv(&args, false),
        vec!["-r", "@", "-c", "abc", "-c", "def"]
    );
}

#[test]
fn remote_emits_long_flag() {
    let args = PushArgs {
        remote: Some("upstream".into()),
        ..empty()
    };
    assert_eq!(push_argv(&args, false), vec!["--remote", "upstream"]);
}

#[test]
fn boolean_flags() {
    let args = PushArgs {
        all: true,
        tracked: true,
        deleted: true,
        ..empty()
    };
    let argv = push_argv(&args, false);
    assert!(argv.contains(&"--all".to_string()));
    assert!(argv.contains(&"--tracked".to_string()));
    assert!(argv.contains(&"--deleted".to_string()));
}

#[test]
fn passthrough_appended_after_known_flags() {
    let args = PushArgs {
        bookmark: vec!["main".into()],
        passthrough: vec!["--option".into(), "foo=bar".into()],
        ..empty()
    };
    let argv = push_argv(&args, false);
    // bookmark first, passthrough last
    assert_eq!(argv[0], "-b");
    assert_eq!(argv[1], "main");
    assert_eq!(argv[argv.len() - 2], "--option");
    assert_eq!(argv[argv.len() - 1], "foo=bar");
}

#[test]
fn dry_run_combines_with_other_flags() {
    let args = PushArgs {
        bookmark: vec!["main".into()],
        ..empty()
    };
    let argv = push_argv(&args, true);
    assert!(argv.contains(&"-b".to_string()));
    assert!(argv.contains(&"main".to_string()));
    assert!(argv.contains(&"--dry-run".to_string()));
}
