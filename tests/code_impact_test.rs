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

/// Mirrors this repo's actual layout: a root package whose manifest_dir
/// IS the workspace root (like the `agentflare` binary crate), which
/// therefore recursively contains another member crate's directory
/// underneath it. A confirm-scan that doesn't attribute each hit back to
/// its true owning crate reports that member's own files twice — once
/// correctly, once again mislabeled as "via the root crate" — exactly the
/// over-broad/duplicate result a live run against the real repo surfaced.
fn build_nested_fixture_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::write(
        root.join("Cargo.toml"),
        r#"[workspace]
members = [".", "crates/lib_a"]
resolver = "2"

[package]
name = "app"
version = "0.1.0"
edition = "2021"

[dependencies]
lib_a = { path = "crates/lib_a" }
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn uses_lib_a() { lib_a::hello(); }\n",
    )
    .unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

    fs::create_dir_all(root.join("crates/lib_a/src")).unwrap();
    fs::create_dir_all(root.join("crates/lib_a/tests")).unwrap();
    fs::write(
        root.join("crates/lib_a/Cargo.toml"),
        "[package]\nname = \"lib_a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(root.join("crates/lib_a/src/lib.rs"), "pub fn hello() {}\n").unwrap();
    fs::write(
        root.join("crates/lib_a/tests/lib_a_test.rs"),
        "use lib_a::hello;\n#[test]\nfn it_works() { hello(); }\n",
    )
    .unwrap();

    dir
}

#[test]
fn impact_never_double_reports_a_nested_crates_own_files_under_an_ancestor_crate() {
    let workspace = build_nested_fixture_workspace();
    let root = workspace.path();

    let report =
        agentflare::code::impact_for_path(root, &root.join("crates/lib_a/src/lib.rs")).unwrap();

    assert_eq!(report.owner_crate, "lib_a");
    let test_hits: Vec<_> = report
        .hits
        .iter()
        .filter(|h| h.file.ends_with("crates/lib_a/tests/lib_a_test.rs"))
        .collect();
    assert_eq!(
        test_hits.len(),
        1,
        "lib_a's own integration test must appear exactly once, got: {test_hits:?}"
    );
    assert!(matches!(
        test_hits[0].reason,
        agentflare::code::ImpactReason::OwnIntegrationTest
    ));
    // The real cross-crate reference (src/lib.rs calling lib_a::hello()) must
    // still be found, attributed to its true owner `app`, not duplicated.
    let src_hits: Vec<_> = report
        .hits
        .iter()
        .filter(|h| h.file.ends_with("src/lib.rs") && !h.file.to_string_lossy().contains("lib_a"))
        .collect();
    assert_eq!(
        src_hits.len(),
        1,
        "app's src/lib.rs reference must appear exactly once, got: {src_hits:?}"
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
