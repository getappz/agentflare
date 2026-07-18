use agentflare_artifacts::{ArtifactServer, ArtifactStore};
use std::sync::Arc;

pub fn serve(host: &str, port: u16, dir: Option<std::path::PathBuf>) {
    let dir = dir.unwrap_or_else(|| crate::paths::home().join(".agentflare").join("artifacts"));
    let store = Arc::new(ArtifactStore::new(dir.clone()));
    let server =
        ArtifactServer::start_on(store, host, port).expect("failed to start artifact server");
    let url = server.base_url();
    crate::ui::info(&format!("agentflare artifacts server listening on {url}"));
    crate::ui::info(&format!("  store: {}", dir.display()));
    if host != "127.0.0.1" && host != "localhost" {
        crate::ui::warning(&format!(
            "bound to {host} — anyone on your network can view these artifacts"
        ));
    }
    loop {
        std::thread::park();
    }
}
