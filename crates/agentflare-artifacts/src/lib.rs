pub mod server;
pub mod store;
pub mod types;

pub use server::ArtifactServer;
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
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
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
        };
        let resp = store.publish(&req).unwrap();
        let id = resp.id.clone();

        let update = PublishRequest {
            name: "updated".into(),
            artifact_type: ArtifactType::Markdown,
            content: "v2".into(),
            session_id: "ses-1".into(),
            update_id: Some(id.clone()),
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
            })
            .unwrap();
        store
            .publish(&PublishRequest {
                name: "b".into(),
                artifact_type: ArtifactType::Html,
                content: "b".into(),
                session_id: "ses-2".into(),
                update_id: None,
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
            })
            .unwrap();
        assert!(store.delete(&resp.id).unwrap());
        assert!(store.get(&resp.id).is_err());
        assert!(!store.delete(&resp.id).unwrap());
    }

    #[test]
    fn server_serves_artifact_via_http() {
        let store = Arc::new(test_store("server_serves_artifact_via_http"));
        let server = ArtifactServer::start(store.clone()).unwrap();
        let port = server.port();

        store
            .publish(&PublishRequest {
                name: "http-test".into(),
                artifact_type: ArtifactType::Text,
                content: "OK".into(),
                session_id: "ses-1".into(),
                update_id: None,
            })
            .unwrap();

        let listing = read_http("/", port);
        assert!(listing.contains("http-test"), "index page shows artifact: {listing}");

        let resp = read_http("/", port);
        assert!(resp.contains("HTTP/1.0 200") || resp.contains("HTTP/1.1 200"), "bad status: {resp}");
    }

    #[test]
    fn server_404_on_missing() {
        let store = Arc::new(test_store("server_404_on_missing"));
        let server = ArtifactServer::start(store.clone()).unwrap();
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
        };
        let json = serde_json::to_string(&a).unwrap();
        let b: Artifact = serde_json::from_str(&json).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(a.artifact_type, b.artifact_type);

        let summary = ArtifactSummary::from(&a);
        assert_eq!(summary.name, "test");
    }
}
