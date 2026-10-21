//! `project` tool actions beyond info/list (archive/unarchive, item #768).
//! Own file: `action_tests.rs` sits at the repo LOC gate.

use super::*;

#[test]
fn project_archive_hides_its_folder_from_discovery_and_unarchive_restores_it() {
    let (_tmp, s) = harness();
    let info: serde_json::Value = serde_json::from_str(
        &s.project(Parameters(ProjectRequest {
            action: "info".into(),
            project: None,
        }))
        .unwrap(),
    )
    .unwrap();
    let project_id = info["id"].as_str().unwrap().to_string();
    let name = info["name"].as_str().unwrap().to_string();
    let registered = |s: &AgentflareMcp| {
        s.with_backend_db(|conn| {
            agentflare_backend::project_dir::list(conn)
                .unwrap()
                .iter()
                .any(|d| d.project_id == project_id)
        })
        .unwrap()
    };
    assert!(registered(&s), "info registers this repo's folder");

    // Archive by name (case-insensitive, like the `project` override).
    let archived: serde_json::Value = serde_json::from_str(
        &s.project(Parameters(ProjectRequest {
            action: "archive".into(),
            project: Some(name.to_uppercase()),
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(archived["id"], project_id);
    assert!(archived["archived_at"].is_i64());
    assert!(!registered(&s), "archived project leaves the sweep list");
    // Still listed, still resolvable -- archive is not delete.
    let listed: serde_json::Value = serde_json::from_str(
        &s.project(Parameters(ProjectRequest {
            action: "list".into(),
            project: None,
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == project_id && p["archived_at"].is_i64())
    );

    // Unarchive by UUID; no re-registration needed.
    let back: serde_json::Value = serde_json::from_str(
        &s.project(Parameters(ProjectRequest {
            action: "unarchive".into(),
            project: Some(project_id.clone()),
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(back["archived_at"].is_null());
    assert!(registered(&s));

    let err = s
        .project(Parameters(ProjectRequest {
            action: "archive".into(),
            project: Some("no-such-project".into()),
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}
