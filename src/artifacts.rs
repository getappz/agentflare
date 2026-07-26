use agentflare_artifacts::{ArtifactServer, ArtifactStore};
use std::sync::Arc;

pub fn serve(host: &str, port: u16, dir: Option<std::path::PathBuf>) {
    let store = if let Some(d) = dir {
        Arc::new(ArtifactStore::new(d))
    } else {
        match crate::store::open() {
            Ok(s) => Arc::new(ArtifactStore::with_store(s)),
            Err(e) => {
                eprintln!("[artifacts] fallback to flat-file store: {e}");
                let d = crate::paths::home().join(".agentflare").join("artifacts");
                Arc::new(ArtifactStore::new(d))
            }
        }
    };
    let server =
        ArtifactServer::start_on(store, host, port).expect("failed to start artifact server");
    let url = server.base_url();
    crate::ui::info(&format!("agentflare artifacts server listening on {url}"));
    if host != "127.0.0.1" && host != "localhost" {
        crate::ui::warning(&format!(
            "bound to {host} — anyone on your network can view these artifacts"
        ));
    }
    loop {
        std::thread::park();
    }
}
