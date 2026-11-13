use indoc::indoc;
use jj_hooks::bookmark_updates::{BookmarkUpdate, UpdateType, parse_git_push_dry_run};

#[test]
fn display_formatting() {
    let move_forward = BookmarkUpdate {
        remote: "origin".into(),
        bookmark: "main".into(),
        update_type: UpdateType::MoveForward,
        old_commit: Some("d964e724c76eaa".into()),
        new_commit: Some("a81d749233ffbb".into()),
    };
    assert_eq!(
        move_forward.to_string(),
        "Move forward main from d964e724 to a81d7492"
    );

    let add = BookmarkUpdate {
        remote: "origin".into(),
        bookmark: "feature".into(),
        update_type: UpdateType::Add,
        old_commit: None,
        new_commit: Some("591f7e9aae85cc".into()),
    };
    assert_eq!(add.to_string(), "Add feature to 591f7e9a");

    let delete = BookmarkUpdate {
        remote: "origin".into(),
        bookmark: "stale".into(),
        update_type: UpdateType::Delete,
        old_commit: Some("9c712e75a982dd".into()),
        new_commit: None,
    };
    assert_eq!(delete.to_string(), "Delete stale from 9c712e75");
}

#[test]
fn modern_format() {
    let output = indoc! {"
        Changes to push to origin:
          bookmark: main [move forward from d964e724c76e to a81d749233ff]
          bookmark: painstaking [add to 591f7e9aae85]
          bookmark: sideways [move sideways from 9c712e75a982 to 23f89ce4b31b]
          bookmark: backward [move backward from d964e724c76e to 561998a40ada]
          bookmark: deleted [delete from 9c712e75a982]
        Dry-run requested, not pushing.
    "};

    assert_eq!(parse_git_push_dry_run(output).unwrap(), expected());
}

#[test]
fn legacy_format() {
    let output = indoc! {"
        Changes to push to origin:
          Move forward bookmark main from d964e724c76e to a81d749233ff
          Add bookmark painstaking to 591f7e9aae85
          Move sideways bookmark sideways from 9c712e75a982 to 23f89ce4b31b
          Move backward bookmark backward from d964e724c76e to 561998a40ada
          Delete bookmark deleted from 9c712e75a982
        Dry-run requested, not pushing.
    "};

    assert_eq!(parse_git_push_dry_run(output).unwrap(), expected());
}

#[test]
fn nothing_to_push() {
    let output = "Nothing changed.\n";
    assert!(parse_git_push_dry_run(output).unwrap().is_empty());
}

#[test]
fn bookmark_line_before_remote_line_is_an_error() {
    let output = indoc! {"
          bookmark: main [move forward from d964e724c76e to a81d749233ff]
        Changes to push to origin:
    "};
    assert!(parse_git_push_dry_run(output).is_err());
}

#[test]
fn second_remote_resets_context() {
    let output = indoc! {"
        Changes to push to origin:
          bookmark: main [move forward from d964e724c76e to a81d749233ff]
        Changes to push to upstream:
          bookmark: feature [add to 591f7e9aae85]
    "};
    let updates = parse_git_push_dry_run(output).unwrap();
    assert_eq!(updates.len(), 2);
    assert!(updates.contains(&BookmarkUpdate {
        remote: "origin".into(),
        bookmark: "main".into(),
        update_type: UpdateType::MoveForward,
        old_commit: Some("d964e724c76e".into()),
        new_commit: Some("a81d749233ff".into()),
    }));
    assert!(updates.contains(&BookmarkUpdate {
        remote: "upstream".into(),
        bookmark: "feature".into(),
        update_type: UpdateType::Add,
        old_commit: None,
        new_commit: Some("591f7e9aae85".into()),
    }));
}

fn expected() -> std::collections::HashSet<BookmarkUpdate> {
    [
        BookmarkUpdate {
            remote: "origin".into(),
            bookmark: "main".into(),
            update_type: UpdateType::MoveForward,
            old_commit: Some("d964e724c76e".into()),
            new_commit: Some("a81d749233ff".into()),
        },
        BookmarkUpdate {
            remote: "origin".into(),
            bookmark: "painstaking".into(),
            update_type: UpdateType::Add,
            old_commit: None,
            new_commit: Some("591f7e9aae85".into()),
        },
        BookmarkUpdate {
            remote: "origin".into(),
            bookmark: "sideways".into(),
            update_type: UpdateType::MoveSideways,
            old_commit: Some("9c712e75a982".into()),
            new_commit: Some("23f89ce4b31b".into()),
        },
        BookmarkUpdate {
            remote: "origin".into(),
            bookmark: "backward".into(),
            update_type: UpdateType::MoveBackward,
            old_commit: Some("d964e724c76e".into()),
            new_commit: Some("561998a40ada".into()),
        },
        BookmarkUpdate {
            remote: "origin".into(),
            bookmark: "deleted".into(),
            update_type: UpdateType::Delete,
            old_commit: Some("9c712e75a982".into()),
            new_commit: None,
        },
    ]
    .into_iter()
    .collect()
}
