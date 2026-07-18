use super::*;

/// Minimal HTTP GET against a `http://127.0.0.1:<port>/<id>` URL,
/// returning the full response (status line + headers + body).
fn http_get(url: &str) -> String {
    use std::io::{Read, Write};
    let rest = url.strip_prefix("http://").expect("http url");
    let (host_port, path) = rest.split_once('/').unwrap_or((rest, ""));
    let mut stream = std::net::TcpStream::connect(host_port)
        .unwrap_or_else(|_| panic!("connect to {host_port}"));
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    write!(stream, "GET /{path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
    stream.flush().unwrap();
    let mut full = String::new();
    let _ = stream.read_to_string(&mut full);
    full
}

#[test]
fn artifact_publish_serves_content_at_returned_url() {
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        artifacts_dir_override: Some(tmp.path().to_path_buf()),
        ..Default::default()
    };
    let out = s
        .artifact(Parameters(ArtifactRequest {
            action: "publish".into(),
            name: Some("hello".into()),
            r#type: None,
            content: Some("artifact-body-marker".into()),
            session_id: None,
            update_id: None,
            ..Default::default()
        }))
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let url = v["url"].as_str().expect("url in response");
    assert!(url.starts_with("http://127.0.0.1:"), "local url: {url}");
    assert!(!v["id"].as_str().unwrap_or_default().is_empty());

    let resp = http_get(url);
    assert!(resp.contains("200"), "serves published artifact: {resp}");
    assert!(
        resp.contains("artifact-body-marker"),
        "body present: {resp}"
    );
}

#[test]
fn artifact_publish_update_id_keeps_same_id() {
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        artifacts_dir_override: Some(tmp.path().to_path_buf()),
        ..Default::default()
    };
    let first: serde_json::Value = serde_json::from_str(
        &s.artifact(Parameters(ArtifactRequest {
            action: "publish".into(),
            name: Some("doc".into()),
            r#type: Some("markdown".into()),
            content: Some("v1".into()),
            session_id: Some("ses-1".into()),
            update_id: None,
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let id = first["id"].as_str().unwrap().to_string();

    let second: serde_json::Value = serde_json::from_str(
        &s.artifact(Parameters(ArtifactRequest {
            action: "publish".into(),
            name: Some("doc".into()),
            r#type: Some("markdown".into()),
            content: Some("v2".into()),
            session_id: Some("ses-1".into()),
            update_id: Some(id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(second["id"].as_str().unwrap(), id);
    assert_eq!(second["url"], first["url"]);
}

#[test]
fn artifact_list_get_delete_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        artifacts_dir_override: Some(tmp.path().to_path_buf()),
        ..Default::default()
    };
    let publish = |name: &str, session: &str| -> serde_json::Value {
        serde_json::from_str(
            &s.artifact(Parameters(ArtifactRequest {
                action: "publish".into(),
                name: Some(name.into()),
                r#type: None,
                content: Some(format!("content-of-{name}")),
                session_id: Some(session.into()),
                update_id: None,
                description: Some(format!("desc-{name}")),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap()
    };
    let a = publish("alpha", "ses-1");
    let _b = publish("beta", "ses-2");

    let all: serde_json::Value = serde_json::from_str(
        &s.artifact(Parameters(ArtifactRequest {
            action: "list".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(all.as_array().unwrap().len(), 2);

    let one: serde_json::Value = serde_json::from_str(
        &s.artifact(Parameters(ArtifactRequest {
            action: "list".into(),
            session_id: Some("ses-1".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(one.as_array().unwrap().len(), 1);
    assert_eq!(one[0]["name"], "alpha");
    assert_eq!(one[0]["description"], "desc-alpha");

    let id = a["id"].as_str().unwrap().to_string();
    let got: serde_json::Value = serde_json::from_str(
        &s.artifact(Parameters(ArtifactRequest {
            action: "get".into(),
            id: Some(id.clone()),
            version: None,
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(got["content"], "content-of-alpha");

    let del: serde_json::Value = serde_json::from_str(
        &s.artifact(Parameters(ArtifactRequest {
            action: "delete".into(),
            id: Some(id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(del["deleted"], id);

    let err = s
        .artifact(Parameters(ArtifactRequest {
            action: "get".into(),
            id: Some(id),
            version: None,
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn artifact_publish_version_and_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        artifacts_dir_override: Some(tmp.path().to_path_buf()),
        ..Default::default()
    };
    let first: serde_json::Value = serde_json::from_str(
        &s.artifact(Parameters(ArtifactRequest {
            action: "publish".into(),
            name: Some("doc".into()),
            r#type: None,
            content: Some("v1".into()),
            session_id: None,
            update_id: None,
            label: Some("draft".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(first["version"], 1);
    let id = first["id"].as_str().unwrap().to_string();

    // stale base_version maps to invalid_params, not internal_error
    let update = |base: Option<u32>, content: &str| {
        s.artifact(Parameters(ArtifactRequest {
            action: "publish".into(),
            name: Some("doc".into()),
            r#type: None,
            content: Some(content.into()),
            session_id: None,
            update_id: Some(id.clone()),
            base_version: base,
            ..Default::default()
        }))
    };
    let second: serde_json::Value = serde_json::from_str(&update(Some(1), "v2").unwrap()).unwrap();
    assert_eq!(second["version"], 2);

    let err = update(Some(1), "v3-stale").unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(err.to_string().contains("conflict"), "{err}");
}

#[test]
fn artifact_list_filters_by_recipient_and_thread() {
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        artifacts_dir_override: Some(tmp.path().to_path_buf()),
        ..Default::default()
    };
    let publish =
        |name: &str, recipient: Option<&str>, thread: Option<&str>| -> serde_json::Value {
            serde_json::from_str(
                &s.artifact(Parameters(ArtifactRequest {
                    action: "publish".into(),
                    name: Some(name.into()),
                    content: Some(format!("content {name}")),
                    recipient: recipient.map(Into::into),
                    thread_id: thread.map(Into::into),
                    ..Default::default()
                }))
                .unwrap(),
            )
            .unwrap()
        };
    publish("packet", Some("codex"), Some("t1"));
    publish("reply", Some("claude-code"), Some("t1"));
    publish("other", None, None);

    let inbox: serde_json::Value = serde_json::from_str(
        &s.artifact(Parameters(ArtifactRequest {
            action: "list".into(),
            inbox_recipient: Some("codex".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(inbox.as_array().unwrap().len(), 1);
    assert_eq!(inbox[0]["name"], "packet");

    let thread: serde_json::Value = serde_json::from_str(
        &s.artifact(Parameters(ArtifactRequest {
            action: "list".into(),
            thread_id: Some("t1".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(thread.as_array().unwrap().len(), 2);
}

fn handoff_harness() -> (tempfile::TempDir, AgentflareMcp) {
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        agent: Some("claude-code".into()),
        ..Default::default()
    };
    (tmp, s)
}

fn item_assets(s: &AgentflareMcp, item_id: &str) -> serde_json::Value {
    serde_json::from_str(
        &s.asset(Parameters(AssetRequest {
            action: "list".into(),
            id: None,
            item_id: Some(item_id.to_string()),
            project_id: None,
            filename: None,
            metadata: None,
        }))
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn handoff_tool_requires_recipient_and_assigns_item() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = handoff_harness();

        // A blank recipient is rejected — the whole reason this tool exists.
        let err = s
            .handoff(Parameters(HandoffRequest {
                recipient: "  ".into(),
                name: "orphan".into(),
                content: "for someone".into(),
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        // A real handoff creates an item assigned to the recipient and
        // attaches the content to it as an asset.
        let result: serde_json::Value = serde_json::from_str(
            &s.handoff(Parameters(HandoffRequest {
                recipient: "opencode".into(),
                name: "review-packet".into(),
                content: "please review".into(),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let item_id = result["item_id"].as_str().unwrap().to_string();
        assert_eq!(result["recipient"], "opencode");
        assert_eq!(result["asset_version"], 1);

        let item: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "get".into(),
                id: Some(item_id.clone()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(item["name"], "review-packet");
        assert_eq!(item["assignee_agent"], "opencode");

        let assets = item_assets(&s, &item_id);
        assert_eq!(assets.as_array().unwrap().len(), 1);
        assert_eq!(assets[0]["filename"], format!("{item_id}.md"));
    });
}

#[test]
fn handoff_trims_whitespace_padded_recipient() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = handoff_harness();

        // A whitespace-padded recipient passes the emptiness check but must
        // still be stored trimmed, or exact-match assignee lookups miss it.
        let result: serde_json::Value = serde_json::from_str(
            &s.handoff(Parameters(HandoffRequest {
                recipient: "  opencode  ".into(),
                name: "review-packet".into(),
                content: "please review".into(),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result["recipient"], "opencode");

        let item_id = result["item_id"].as_str().unwrap().to_string();
        let item: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "get".into(),
                id: Some(item_id),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(item["assignee_agent"], "opencode");
    });
}

#[test]
fn handoff_with_item_id_assigns_existing_item_and_versions_the_asset() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = handoff_harness();
        let created: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(empty_item_create("Existing task")))
                .unwrap(),
        )
        .unwrap();
        let item_id = created["id"].as_str().unwrap().to_string();

        let first: serde_json::Value = serde_json::from_str(
            &s.handoff(Parameters(HandoffRequest {
                recipient: "opencode".into(),
                name: "Existing task".into(),
                content: "v1 content".into(),
                item_id: Some(item_id.clone()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(first["item_id"], item_id);
        assert_eq!(first["asset_version"], 1);

        // A different brief/name on the reply must not reset the version
        // chain — it's keyed on item_id, not name.
        let second: serde_json::Value = serde_json::from_str(
            &s.handoff(Parameters(HandoffRequest {
                recipient: "opencode".into(),
                name: "Addressed feedback".into(),
                content: "v2 content".into(),
                item_id: Some(item_id.clone()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(second["asset_version"], 2);

        // no duplicate item was created
        let item: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "get".into(),
                id: Some(item_id.clone()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(item["assignee_agent"], "opencode");
        assert_eq!(item_assets(&s, &item_id).as_array().unwrap().len(), 2);
    });
}

#[test]
fn artifact_diff_tool_returns_unified_diff() {
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        artifacts_dir_override: Some(tmp.path().to_path_buf()),
        ..Default::default()
    };
    let first: serde_json::Value = serde_json::from_str(
        &s.artifact(Parameters(ArtifactRequest {
            action: "publish".into(),
            name: Some("doc".into()),
            content: Some("alpha\nbeta\n".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let id = first["id"].as_str().unwrap().to_string();
    s.artifact(Parameters(ArtifactRequest {
        action: "publish".into(),
        name: Some("doc".into()),
        content: Some("alpha\ngamma\n".into()),
        update_id: Some(id.clone()),
        ..Default::default()
    }))
    .unwrap();

    // to_version omitted = latest
    let diff = s
        .artifact(Parameters(ArtifactRequest {
            action: "diff".into(),
            id: Some(id),
            from_version: Some(1),
            to_version: None,
            ..Default::default()
        }))
        .unwrap();
    assert!(diff.contains("-beta"), "{diff}");
    assert!(diff.contains("+gamma"), "{diff}");
}

#[test]
fn artifact_search_matches_name_description_and_content() {
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        artifacts_dir_override: Some(tmp.path().to_path_buf()),
        ..Default::default()
    };
    s.artifact(Parameters(ArtifactRequest {
        action: "publish".into(),
        name: Some("alpha".into()),
        content: Some("there is a hidden NEEDLE in here".into()),
        ..Default::default()
    }))
    .unwrap();
    s.artifact(Parameters(ArtifactRequest {
        action: "publish".into(),
        name: Some("beta".into()),
        content: Some("nothing to see".into()),
        ..Default::default()
    }))
    .unwrap();

    let hits: serde_json::Value = serde_json::from_str(
        &s.artifact(Parameters(ArtifactRequest {
            action: "search".into(),
            query: Some("needle".into()),
            session_id: None,
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(hits.as_array().unwrap().len(), 1);
    assert_eq!(hits[0]["name"], "alpha");
    assert!(
        hits[0]["snippet"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("needle"),
        "{hits}"
    );

    let by_name: serde_json::Value = serde_json::from_str(
        &s.artifact(Parameters(ArtifactRequest {
            action: "search".into(),
            query: Some("beta".into()),
            session_id: None,
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(by_name.as_array().unwrap().len(), 1);
}

#[test]
fn artifact_publish_captures_git_provenance_in_repo() {
    // Tests run with cwd inside this git repo, so capture must succeed.
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        artifacts_dir_override: Some(tmp.path().to_path_buf()),
        ..Default::default()
    };
    let out: serde_json::Value = serde_json::from_str(
        &s.artifact(Parameters(ArtifactRequest {
            action: "publish".into(),
            name: Some("prov".into()),
            content: Some("x".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let got: serde_json::Value = serde_json::from_str(
        &s.artifact(Parameters(ArtifactRequest {
            action: "get".into(),
            id: Some(out["id"].as_str().unwrap().into()),
            version: None,
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let commit = got["git"]["commit"].as_str().expect("git commit captured");
    assert!(commit.len() >= 7, "{got}");
}

#[test]
fn artifact_publish_defaults_sender_to_agent_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        artifacts_dir_override: Some(tmp.path().to_path_buf()),
        agent: Some("opencode".into()),
        ..Default::default()
    };
    let sender_of = |req: ArtifactRequest| -> serde_json::Value {
        let out: serde_json::Value =
            serde_json::from_str(&s.artifact(Parameters(req)).unwrap()).unwrap();
        let got: serde_json::Value = serde_json::from_str(
            &s.artifact(Parameters(ArtifactRequest {
                action: "get".into(),
                id: Some(out["id"].as_str().unwrap().into()),
                version: None,
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        got["sender"].clone()
    };

    let defaulted = sender_of(ArtifactRequest {
        action: "publish".into(),
        name: Some("defaulted".into()),
        content: Some("x".into()),
        ..Default::default()
    });
    assert_eq!(defaulted, "opencode");

    // ArtifactRequest has no `sender` field (removed in #75): authorship is
    // always the server-derived identity, so a caller cannot attribute a
    // published artifact to another agent. The spoof is unrepresentable at
    // the type level — stronger than a runtime "override ignored" check.
}

#[test]
fn identity_prefers_explicit_override_then_detection() {
    // Explicit override beats detection…
    assert_eq!(
        AgentflareMcp::identity(Some("opencode".into())).as_deref(),
        Some("opencode")
    );
    // …empty counts as unset, and without an override identity falls
    // back to detecting the host that launched us (None outside agents).
    assert_eq!(
        AgentflareMcp::identity(Some(String::new())),
        agent_detector::agent_name()
    );
    assert_eq!(AgentflareMcp::identity(None), agent_detector::agent_name());
}

#[test]
fn artifact_publish_rejects_empty_name_and_content() {
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        artifacts_dir_override: Some(tmp.path().to_path_buf()),
        ..Default::default()
    };
    for (name, content) in [("", "x"), ("x", "")] {
        let err = s
            .artifact(Parameters(ArtifactRequest {
                action: "publish".into(),
                name: Some(name.into()),
                r#type: None,
                content: Some(content.into()),
                session_id: None,
                update_id: None,
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }
}
