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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn project_rename_and_delete() {
    let (_tmp, s) = harness();
    let info: serde_json::Value = serde_json::from_str(
        &s.project(Parameters(ProjectRequest {
            action: "info".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let project_id = info["id"].as_str().unwrap().to_string();

    // 1. Rename project
    let renamed: serde_json::Value = serde_json::from_str(
        &s.project(Parameters(ProjectRequest {
            action: "rename".into(),
            project: Some(project_id.clone()),
            name: Some("appz-cli-renamed".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(renamed["id"], project_id);
    assert_eq!(renamed["name"], "appz-cli-renamed");

    // 2. Delete project
    let deleted: serde_json::Value = serde_json::from_str(
        &s.project(Parameters(ProjectRequest {
            action: "delete".into(),
            project: Some(project_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(deleted["status"], "deleted");
    assert_eq!(deleted["id"], project_id);
}

#[test]
fn project_rename_explicit_identifier_collision_errors() {
    let (tmp, s) = harness();
    let conn = backend_conn(&tmp);
    let first = s.resolve_project(&conn).unwrap();
    let ws = first.workspace_id.clone();
    let taken = agentflare_backend::project::create(
        &conn,
        agentflare_backend::project::CreateProject {
            workspace_id: ws,
            name: "other-project".into(),
            identifier: "TAKEN".into(),
            external_source: None,
            external_id: None,
        },
    )
    .unwrap();
    assert_eq!(taken.identifier, "TAKEN");

    let err = s
        .project(Parameters(ProjectRequest {
            action: "rename".into(),
            project: Some(first.id.clone()),
            name: Some("renamed-first".into()),
            identifier: Some("TAKEN".into()),
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);

    let still = agentflare_backend::project::get(&conn, &first.id).unwrap();
    assert_eq!(still.name, first.name, "failed rename must not change name");
    assert_eq!(
        still.identifier, first.identifier,
        "failed rename must not change identifier"
    );
}

#[test]
fn project_rename_derived_identifier_collision_keeps_existing() {
    let (tmp, s) = harness();
    let conn = backend_conn(&tmp);
    let first = s.resolve_project(&conn).unwrap();
    let ws = first.workspace_id.clone();
    agentflare_backend::project::create(
        &conn,
        agentflare_backend::project::CreateProject {
            workspace_id: ws,
            name: "holder".into(),
            identifier: "COLLIDE".into(),
            external_source: None,
            external_id: None,
        },
    )
    .unwrap();

    // Name "collide" derives to "COLLIDE"; collision fallback keeps first's identifier.
    let renamed: serde_json::Value = serde_json::from_str(
        &s.project(Parameters(ProjectRequest {
            action: "rename".into(),
            project: Some(first.id.clone()),
            name: Some("collide".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(renamed["name"], "collide");
    assert_eq!(renamed["identifier"], first.identifier);
}
