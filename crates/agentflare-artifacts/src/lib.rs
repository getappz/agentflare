pub mod server;
pub mod store;
pub mod types;

pub use server::{
    probe, probe_path, render_artifact_page, render_index, valid_id, ArtifactServer, DEFAULT_PORT,
};
pub use store::ArtifactStore;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;
    use std::net::TcpStream;
    use std::sync::Arc;
    use std::time::Duration;

    fn test_store(name: &str) -> ArtifactStore {
        let dir = std::env::temp_dir().join(format!("agentflare-artifacts-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        ArtifactStore::new(dir)
    }

    fn read_http(path: &str, port: u16) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port))
            .unwrap_or_else(|_| panic!("connect to :{port}"));
        let req = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
        use std::io::Read;
        use std::io::Write;
        stream.write_all(req.as_bytes()).unwrap();
        stream.flush().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(&stream);
        let mut full = String::new();
        let _ = reader.read_to_string(&mut full);
        full
    }

    #[test]
    fn publish_and_get_artifact() {
        let store = test_store("publish_and_get_artifact");
        let req = PublishRequest {
            name: "hello".into(),
            artifact_type: ArtifactType::Text,
            content: "Hello, world!".into(),
            session_id: "ses-1".into(),
            update_id: None,
            ..Default::default()
        };
        let resp = store.publish(&req).unwrap();
        assert!(!resp.id.is_empty());
        assert_eq!(resp.url, format!("/{}", resp.id));

        let artifact = store.get(&resp.id).unwrap();
        assert_eq!(artifact.name, "hello");
        assert_eq!(artifact.content, "Hello, world!");
        assert_eq!(artifact.artifact_type, ArtifactType::Text);
    }

    #[test]
    fn update_existing_artifact() {
        let store = test_store("update_existing_artifact");
        let req = PublishRequest {
            name: "original".into(),
            artifact_type: ArtifactType::Text,
            content: "v1".into(),
            session_id: "ses-1".into(),
            update_id: None,
            ..Default::default()
        };
        let resp = store.publish(&req).unwrap();
        let id = resp.id.clone();

        let update = PublishRequest {
            name: "updated".into(),
            artifact_type: ArtifactType::Markdown,
            content: "v2".into(),
            session_id: "ses-1".into(),
            update_id: Some(id.clone()),
            ..Default::default()
        };
        let resp2 = store.publish(&update).unwrap();
        assert_eq!(resp2.id, id);

        let artifact = store.get(&id).unwrap();
        assert_eq!(artifact.content, "v2");
        assert_eq!(artifact.artifact_type, ArtifactType::Markdown);
        // created_at must stay the same on update
        assert_eq!(artifact.created_at, artifact.updated_at); // same sec
    }

    #[test]
    fn list_artifacts_filtered_by_session() {
        let store = test_store("list_artifacts_filtered_by_session");
        store
            .publish(&PublishRequest {
                name: "a".into(),
                artifact_type: ArtifactType::Text,
                content: "a".into(),
                session_id: "ses-1".into(),
                update_id: None,
                ..Default::default()
            })
            .unwrap();
        store
            .publish(&PublishRequest {
                name: "b".into(),
                artifact_type: ArtifactType::Html,
                content: "b".into(),
                session_id: "ses-2".into(),
                update_id: None,
                ..Default::default()
            })
            .unwrap();

        let all = store.list(None).unwrap();
        assert_eq!(all.len(), 2);

        let s1 = store.list(Some("ses-1")).unwrap();
        assert_eq!(s1.len(), 1);
        assert_eq!(s1[0].name, "a");
    }

    #[test]
    fn delete_artifact() {
        let store = test_store("delete_artifact");
        let resp = store
            .publish(&PublishRequest {
                name: "del".into(),
                artifact_type: ArtifactType::Text,
                content: "x".into(),
                session_id: "s".into(),
                update_id: None,
                ..Default::default()
            })
            .unwrap();
        assert!(store.delete(&resp.id).unwrap());
        assert!(store.get(&resp.id).is_err());
        assert!(!store.delete(&resp.id).unwrap());
    }

    #[test]
    fn server_serves_artifact_via_http() {
        let store = Arc::new(test_store("server_serves_artifact_via_http"));
        let server = ArtifactServer::start(store.clone(), 0).unwrap();
        let port = server.port();

        store
            .publish(&PublishRequest {
                name: "http-test".into(),
                artifact_type: ArtifactType::Text,
                content: "OK".into(),
                session_id: "ses-1".into(),
                update_id: None,
                ..Default::default()
            })
            .unwrap();

        let listing = read_http("/", port);
        assert!(
            listing.contains("http-test"),
            "index page shows artifact: {listing}"
        );

        let resp = read_http("/", port);
        assert!(
            resp.contains("HTTP/1.0 200") || resp.contains("HTTP/1.1 200"),
            "bad status: {resp}"
        );
    }

    #[test]
    fn server_404_on_missing() {
        let store = Arc::new(test_store("server_404_on_missing"));
        let server = ArtifactServer::start(store.clone(), 0).unwrap();
        let resp = read_http("/nonexistent", server.port());
        assert!(
            resp.contains("404") || resp.contains("Not Found"),
            "expected 404: {resp}"
        );
    }

    #[test]
    fn types_serde_roundtrip() {
        let a = Artifact {
            id: "x".into(),
            name: "test".into(),
            artifact_type: ArtifactType::Html,
            content: "<p>hi</p>".into(),
            session_id: "s".into(),
            created_at: 100,
            updated_at: 200,
            version: 1,
            description: None,
            favicon: None,
            sender: None,
            recipient: None,
            thread_id: None,
            reply_to: None,
            git: None,
        };
        let json = serde_json::to_string(&a).unwrap();
        let b: Artifact = serde_json::from_str(&json).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(a.artifact_type, b.artifact_type);

        let summary = ArtifactSummary::from(&a);
        assert_eq!(summary.name, "test");
    }

    #[test]
    fn server_binds_preferred_port() {
        let store = Arc::new(test_store("server_binds_preferred_port"));
        // grab a free port from the OS, release it, then ask the server for it
        let free = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let server = ArtifactServer::start(store, free).unwrap();
        assert_eq!(server.port(), free);

        let resp = read_http("/", free);
        assert!(
            resp.contains("HTTP/1.0 200") || resp.contains("HTTP/1.1 200"),
            "server responds on preferred port: {resp}"
        );
    }

    #[test]
    fn publish_creates_version_history() {
        let store = test_store("publish_creates_version_history");
        let resp = store
            .publish(&PublishRequest {
                name: "doc".into(),
                content: "v1-content".into(),
                session_id: "s".into(),
                label: Some("first".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(resp.version, 1);
        let id = resp.id;

        let resp2 = store
            .publish(&PublishRequest {
                name: "doc".into(),
                content: "v2-content".into(),
                session_id: "s".into(),
                update_id: Some(id.clone()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(resp2.version, 2);

        let history = store.versions(&id).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].version, 1);
        assert_eq!(history[0].label.as_deref(), Some("first"));
        assert_eq!(history[1].version, 2);

        // old version content stays retrievable; current get() is the latest
        assert_eq!(store.get_version(&id, 1).unwrap().content, "v1-content");
        let current = store.get(&id).unwrap();
        assert_eq!(current.content, "v2-content");
        assert_eq!(current.version, 2);
    }

    #[test]
    fn publish_stale_base_version_conflicts() {
        let store = test_store("publish_stale_base_version_conflicts");
        let id = store
            .publish(&PublishRequest {
                name: "cas".into(),
                content: "v1".into(),
                session_id: "s".into(),
                ..Default::default()
            })
            .unwrap()
            .id;

        // correct base succeeds
        let ok = store.publish(&PublishRequest {
            name: "cas".into(),
            content: "v2".into(),
            session_id: "s".into(),
            update_id: Some(id.clone()),
            base_version: Some(1),
            ..Default::default()
        });
        assert_eq!(ok.unwrap().version, 2);

        // stale base (still 1, current is 2) conflicts
        let err = store
            .publish(&PublishRequest {
                name: "cas".into(),
                content: "v3-lost-update".into(),
                session_id: "s".into(),
                update_id: Some(id.clone()),
                base_version: Some(1),
                ..Default::default()
            })
            .unwrap_err();
        assert!(err.to_string().contains("conflict"), "{err}");
        assert_eq!(store.get(&id).unwrap().content, "v2");

        // no base_version = last-write-wins, still allowed
        let forced = store
            .publish(&PublishRequest {
                name: "cas".into(),
                content: "v3".into(),
                session_id: "s".into(),
                update_id: Some(id.clone()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(forced.version, 3);
    }

    fn served_store(name: &str) -> (Arc<ArtifactStore>, u16) {
        let store = Arc::new(test_store(name));
        let server = ArtifactServer::start(store.clone(), 0).unwrap();
        let port = server.port();
        std::mem::forget(server); // keep listener alive for the test duration
        (store, port)
    }

    #[test]
    fn server_rejects_path_traversal_ids() {
        let (store, port) = served_store("server_rejects_path_traversal_ids");
        // Plant an artifact-shaped dir OUTSIDE the store root: base is
        // <dir>/artifacts, so "../escape" reaches <dir>/escape via join.
        let outside = store.base_path().parent().unwrap().join("escape");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("meta.json"),
            r#"{"id":"escape","name":"loot","artifact_type":"text","session_id":"s","created_at":1,"updated_at":1}"#,
        )
        .unwrap();
        std::fs::write(outside.join("content"), "ESCAPED-FILE-CONTENT").unwrap();

        for path in ["/../escape", "/a/../../escape", "/a\\..\\escape"] {
            let resp = read_http(path, port);
            assert!(
                !resp.contains("ESCAPED-FILE-CONTENT"),
                "{path} must not serve outside the store: {resp}"
            );
            assert!(
                resp.contains("404") || resp.contains("Not Found"),
                "{path} must 404: {resp}"
            );
        }
    }

    #[test]
    fn server_escapes_html_in_names_and_text_content() {
        let (store, port) = served_store("server_escapes_html_in_names_and_text");
        let resp = store
            .publish(&PublishRequest {
                name: "<script>alert('name')</script>".into(),
                content: "</pre><script>alert('content')</script>".into(),
                session_id: "s".into(),
                ..Default::default()
            })
            .unwrap();

        let page = read_http(&format!("/{}", resp.id), port);
        assert!(
            !page.contains("<script>alert"),
            "raw script must not survive: {page}"
        );
        assert!(
            page.contains("&lt;script&gt;"),
            "escaped form present: {page}"
        );

        let index = read_http("/", port);
        assert!(
            !index.contains("<script>alert"),
            "index escapes names: {index}"
        );
    }

    #[test]
    fn server_serves_version_snapshots_and_history() {
        let (store, port) = served_store("server_serves_version_snapshots");
        let id = store
            .publish(&PublishRequest {
                name: "vdoc".into(),
                content: "OLD-CONTENT".into(),
                session_id: "s".into(),
                label: Some("draft".into()),
                ..Default::default()
            })
            .unwrap()
            .id;
        store
            .publish(&PublishRequest {
                name: "vdoc".into(),
                content: "NEW-CONTENT".into(),
                session_id: "s".into(),
                update_id: Some(id.clone()),
                ..Default::default()
            })
            .unwrap();

        let current = read_http(&format!("/{id}"), port);
        assert!(current.contains("NEW-CONTENT"), "{current}");

        let old = read_http(&format!("/{id}/v/1"), port);
        assert!(old.contains("OLD-CONTENT"), "{old}");

        let history = read_http(&format!("/{id}/versions"), port);
        assert!(
            history.contains("\"version\": 1") || history.contains("\"version\":1"),
            "{history}"
        );
        assert!(history.contains("draft"), "label in history: {history}");
    }

    #[test]
    fn markdown_and_mermaid_pages_render_client_side() {
        let (store, port) = served_store("markdown_and_mermaid_render");
        let md = store
            .publish(&PublishRequest {
                name: "md-doc".into(),
                artifact_type: ArtifactType::Markdown,
                content: "# Title".into(),
                session_id: "s".into(),
                ..Default::default()
            })
            .unwrap();
        let page = read_http(&format!("/{}", md.id), port);
        assert!(
            page.contains("marked"),
            "markdown page loads renderer: {page}"
        );

        let mm = store
            .publish(&PublishRequest {
                name: "mm-doc".into(),
                artifact_type: ArtifactType::Mermaid,
                content: "graph TD; A-->B;".into(),
                session_id: "s".into(),
                ..Default::default()
            })
            .unwrap();
        let page = read_http(&format!("/{}", mm.id), port);
        assert!(
            page.contains("mermaid"),
            "mermaid page loads renderer: {page}"
        );
    }

    #[test]
    fn server_start_on_binds_requested_host() {
        let store = Arc::new(test_store("server_start_on_binds_requested_host"));
        let server = ArtifactServer::start_on(store, "0.0.0.0", 0).unwrap();
        assert!(server.base_url().contains("0.0.0.0"));
        // bound on all interfaces — loopback must reach it
        let resp = read_http("/", server.port());
        assert!(resp.contains("200"), "{resp}");
    }

    #[test]
    fn handoff_envelope_roundtrips() {
        let store = test_store("handoff_envelope_roundtrips");
        let resp = store
            .publish(&PublishRequest {
                name: "review-packet".into(),
                content: "please review".into(),
                session_id: "s".into(),
                sender: Some("claude-code".into()),
                recipient: Some("codex".into()),
                thread_id: Some("thread-7".into()),
                ..Default::default()
            })
            .unwrap();

        let got = store.get(&resp.id).unwrap();
        assert_eq!(got.sender.as_deref(), Some("claude-code"));
        assert_eq!(got.recipient.as_deref(), Some("codex"));
        assert_eq!(got.thread_id.as_deref(), Some("thread-7"));
        assert_eq!(got.reply_to, None);

        // a reply in the same thread carries lineage
        let reply = store
            .publish(&PublishRequest {
                name: "review-reply".into(),
                content: "looks good".into(),
                session_id: "s".into(),
                sender: Some("codex".into()),
                recipient: Some("claude-code".into()),
                thread_id: Some("thread-7".into()),
                reply_to: Some(resp.id.clone()),
                ..Default::default()
            })
            .unwrap();
        let summaries = store.list(None).unwrap();
        let reply_summary = summaries.iter().find(|s| s.id == reply.id).unwrap();
        assert_eq!(reply_summary.reply_to.as_deref(), Some(resp.id.as_str()));
        assert_eq!(reply_summary.recipient.as_deref(), Some("claude-code"));
    }

    #[test]
    fn identical_content_update_is_deduped() {
        let store = test_store("identical_content_update_is_deduped");
        let id = store
            .publish(&PublishRequest {
                name: "doc".into(),
                content: "same-content".into(),
                session_id: "s".into(),
                ..Default::default()
            })
            .unwrap()
            .id;

        // same content again: no new version, metadata still updatable
        let resp = store
            .publish(&PublishRequest {
                name: "doc".into(),
                content: "same-content".into(),
                session_id: "s".into(),
                update_id: Some(id.clone()),
                description: Some("added later".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            resp.version, 1,
            "identical content must not bump the version"
        );
        assert_eq!(store.versions(&id).unwrap().len(), 1);
        assert_eq!(
            store.get(&id).unwrap().description.as_deref(),
            Some("added later")
        );

        // different content still bumps
        let resp = store
            .publish(&PublishRequest {
                name: "doc".into(),
                content: "changed-content".into(),
                session_id: "s".into(),
                update_id: Some(id.clone()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(resp.version, 2);
        assert_eq!(store.versions(&id).unwrap().len(), 2);
    }

    #[test]
    fn diff_between_versions() {
        let store = test_store("diff_between_versions");
        let id = store
            .publish(&PublishRequest {
                name: "doc".into(),
                content: "line one\nline two\n".into(),
                session_id: "s".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        store
            .publish(&PublishRequest {
                name: "doc".into(),
                content: "line one\nline 2\nline three\n".into(),
                session_id: "s".into(),
                update_id: Some(id.clone()),
                ..Default::default()
            })
            .unwrap();

        let diff = store.diff(&id, 1, 2).unwrap();
        assert!(diff.contains("-line two"), "{diff}");
        assert!(diff.contains("+line 2"), "{diff}");
        assert!(diff.contains("+line three"), "{diff}");
    }

    #[test]
    fn index_gallery_shows_metadata() {
        let (store, port) = served_store("index_gallery_shows_metadata");
        store
            .publish(&PublishRequest {
                name: "fancy".into(),
                content: "x".into(),
                session_id: "ses-42".into(),
                description: Some("a fancy description".into()),
                favicon: Some("📊".into()),
                ..Default::default()
            })
            .unwrap();
        let index = read_http("/", port);
        assert!(index.contains("a fancy description"), "{index}");
        assert!(index.contains("📊"), "{index}");
        assert!(index.contains("ses-42"), "grouped by session: {index}");
    }

    #[test]
    fn probe_detects_artifact_server_only() {
        let (_store, port) = served_store("probe_detects_artifact_server_only");
        assert!(probe(port), "running artifact server must probe true");

        // nothing listening: bind to learn a free port, release it, probe it
        let free = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert!(!probe(free), "free port must probe false");

        // a listener that answers HTTP but is not an artifact server
        let foreign = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let fport = foreign.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = foreign.accept() {
                use std::io::Write;
                let _ = s.write_all(b"HTTP/1.0 200 OK\r\n\r\nhello");
            }
        });
        assert!(!probe(fport), "foreign http server must probe false");
    }

    #[test]
    fn sse_live_reload_fires_for_cross_process_publish() {
        let (store, port) = served_store("sse_cross_process_publish");
        let id = store
            .publish(&PublishRequest {
                name: "doc".into(),
                content: "v1".into(),
                session_id: "s".into(),
                ..Default::default()
            })
            .unwrap()
            .id;

        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        use std::io::{Read, Write};
        write!(stream, "GET /{id}/live HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
        stream.flush().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();

        // Wait for the SSE headers (and a beat for the handler to snapshot
        // its baseline version) before publishing, else the handler's first
        // look at the store already sees v2 and no change is ever detected.
        let mut header_buf = [0u8; 512];
        let n = stream.read(&mut header_buf).unwrap();
        assert!(n > 0, "SSE headers must arrive");
        std::thread::sleep(Duration::from_millis(300));

        // Publish v2 through a SECOND store instance on the same directory:
        // its broadcast channel never reaches the serving store's SSE
        // subscribers, exactly like a publish from another process. Only
        // the poll fallback can deliver this one.
        let other = ArtifactStore::new(store.base_path().parent().unwrap().to_path_buf());
        other
            .publish(&PublishRequest {
                name: "doc".into(),
                content: "v2".into(),
                session_id: "s".into(),
                update_id: Some(id.clone()),
                ..Default::default()
            })
            .unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        let mut buf = String::new();
        let mut bytes = [0u8; 1024];
        while std::time::Instant::now() < deadline && !buf.contains("event:") {
            match stream.read(&mut bytes) {
                Ok(0) => break,
                Ok(n) => buf.push_str(&String::from_utf8_lossy(&bytes[..n])),
                Err(_) => break,
            }
        }
        assert!(
            buf.contains("event:"),
            "poll fallback must emit an update event: {buf}"
        );
    }
}
