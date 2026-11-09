mod harness;

use harness::{TestRepo, show};
use std::os::unix::fs::PermissionsExt;

#[test]
fn explicit_from_uses_release_parent_of_merge() {
    let repo = TestRepo::new();
    let base = repo.commit_id_of("main");
    repo.write("release-only", "release\n");
    assert!(repo.jj(&["commit", "-m", "release"]).status.success());
    let release = repo.commit_id_of("@-");
    assert!(repo.jj(&["new", &base]).status.success());
    repo.write("upstream-only", "upstream\n");
    assert!(repo.jj(&["commit", "-m", "upstream"]).status.success());
    let upstream = repo.commit_id_of("@-");
    assert!(repo.jj(&["new", &release, &upstream]).status.success());
    repo.write("hk.pkl", "fixture\n");
    std::fs::remove_file(repo.primary().join("release-only")).unwrap();
    assert!(repo.jj(&["commit", "-m", "merge"]).status.success());
    let merge = repo.commit_id_of("@-");
    let runner = repo.tmp.path().join("hk-runner");
    let output = repo.tmp.path().join("diff");
    std::fs::write(
        &runner,
        "#!/bin/sh\ngit diff --name-only \"$4\" \"$6\" > \"$OUT\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).unwrap();
    let value = format!("\"{}\"", runner.display());
    assert!(
        repo.jj(&["config", "set", "--repo", "jj-hooks.runner-bin.hk", &value])
            .status
            .success()
    );
    let result = repo.tmp.path().join("result.json");
    let out = repo.jj_hooks_with_env(
        &[
            "run",
            "--stage",
            "pre-push",
            "--from",
            &release,
            "--to",
            &merge,
            "--result-json",
            result.to_str().unwrap(),
        ],
        &[("OUT", output.to_str().unwrap())],
    );
    assert!(out.status.success(), "{}", show(&out));
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(result).unwrap()).unwrap();
    assert_eq!(value["from"], release);
    let changed = std::fs::read_to_string(output).unwrap();
    assert!(changed.contains("release-only"));
}
