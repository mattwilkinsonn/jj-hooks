//! Integration tests pinning the `--version` output of both binaries
//! to their own name. Without the argv[0] dispatch in `run()`,
//! `jj-hp --version` prints `jj-hooks 0.3.x` (the clap `name =
//! "jj-hooks"` literal from the shared `Cli` struct), which breaks
//! the homebrew tap formula test in CI on every release. Pinning
//! this here so a future refactor of the dispatch can't silently
//! regress it without surfacing a unit-test failure.

use std::process::Command;

#[test]
fn jj_hooks_version_self_identifies() {
    let bin = env!("CARGO_BIN_EXE_jj-hooks");
    let output = Command::new(bin)
        .arg("--version")
        .output()
        .expect("invoke jj-hooks --version");

    assert!(
        output.status.success(),
        "jj-hooks --version exited non-zero: {:?}",
        output
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    assert!(
        stdout.starts_with("jj-hooks "),
        "jj-hooks --version should print 'jj-hooks <version>', got: {stdout:?}"
    );
}

#[test]
fn jj_hp_version_self_identifies() {
    let bin = env!("CARGO_BIN_EXE_jj-hp");
    let output = Command::new(bin)
        .arg("--version")
        .output()
        .expect("invoke jj-hp --version");

    assert!(
        output.status.success(),
        "jj-hp --version exited non-zero: {:?}",
        output
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    assert!(
        stdout.starts_with("jj-hp "),
        "jj-hp --version should print 'jj-hp <version>', got: {stdout:?}"
    );
}
