use std::fs;
use std::path::Path;

/// Builds a throwaway 2-crate Cargo workspace on disk, mirroring the exact
/// shape of the bug this feature fixes: two crates each with their own
/// `supervisor.rs`, where only one has a `use jobs_crate::Supervisor` — a
/// resolver that matches by basename alone would conflate the two files.
fn build_fixture_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::write(
        root.join("Cargo.toml"),
        r#"[workspace]
members = ["crates/top", "crates/jobs"]
resolver = "2"
"#,
    )
    .unwrap();

    // `top` crate: has its own unrelated `supervisor.rs` (the file-name
    // collision) and does NOT depend on `jobs`.
    fs::create_dir_all(root.join("crates/top/src")).unwrap();
    fs::write(
        root.join("crates/top/Cargo.toml"),
        "[package]\nname = \"top\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(root.join("crates/top/src/lib.rs"), "pub fn top() {}\n").unwrap();
    fs::write(
        root.join("crates/top/src/supervisor.rs"),
        "pub struct UnrelatedSupervisor;\n",
    )
    .unwrap();

    // `jobs` crate: has its own `supervisor.rs` (the real target) and its
    // own integration test that imports it via the crate name — exactly
    // `crates/agentflare-jobs/tests/supervisor_test.rs`'s shape in this repo.
    fs::create_dir_all(root.join("crates/jobs/src")).unwrap();
    fs::create_dir_all(root.join("crates/jobs/tests")).unwrap();
    fs::write(
        root.join("crates/jobs/Cargo.toml"),
        "[package]\nname = \"jobs\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        root.join("crates/jobs/src/lib.rs"),
        "pub mod supervisor;\npub use supervisor::Supervisor;\n",
    )
    .unwrap();
    fs::write(
        root.join("crates/jobs/src/supervisor.rs"),
        "pub struct Supervisor;\n",
    )
    .unwrap();
    fs::write(
        root.join("crates/jobs/tests/supervisor_test.rs"),
        "use jobs::Supervisor;\n#[test]\nfn it_exists() { let _ = Supervisor; }\n",
    )
    .unwrap();

    dir
}

#[test]
fn impact_of_the_real_supervisor_finds_its_own_integration_test_not_the_unrelated_one() {
    let workspace = build_fixture_workspace();
    let root = workspace.path();

    let report =
        agentflare::code::impact_for_path(root, &root.join("crates/jobs/src/supervisor.rs"))
            .unwrap();

    assert_eq!(report.owner_crate, "jobs");
    let test_hit = report
        .hits
        .iter()
        .find(|h| h.file.ends_with("crates/jobs/tests/supervisor_test.rs"));
    assert!(
        test_hit.is_some(),
        "expected jobs/tests/supervisor_test.rs in hits, got: {:?}",
        report.hits.iter().map(|h| &h.file).collect::<Vec<_>>()
    );
    // The unrelated same-named file's crate must never appear as a hit.
    assert!(
        report
            .hits
            .iter()
            .all(|h| !h.file.ends_with("crates/top/src/supervisor.rs"))
    );
}

#[test]
fn impact_of_a_file_outside_any_workspace_member_is_a_clear_error() {
    let workspace = build_fixture_workspace();
    let root = workspace.path();
    let err =
        agentflare::code::impact_for_path(root, Path::new("/tmp/not-in-workspace.rs")).unwrap_err();
    assert!(matches!(
        err,
        agentflare::code::ImpactError::NotInWorkspace(_)
    ));
}

#[test]
fn impact_with_no_cargo_toml_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let err = agentflare::code::impact_for_path(dir.path(), &dir.path().join("x.rs")).unwrap_err();
    assert!(matches!(
        err,
        agentflare::code::ImpactError::Graph(
            agentflare::code::workspace_graph::WorkspaceGraphError::NoCargoToml
        )
    ));
}
