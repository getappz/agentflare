use agentflare_artifacts::{ArtifactServer, ArtifactStore};
use std::sync::Arc;

pub fn serve(port: u16, dir: Option<std::path::PathBuf>) {
    let dir = dir.unwrap_or_else(|| crate::paths::home().join(".agentflare").join("artifacts"));
    let store = Arc::new(ArtifactStore::new(dir.clone()));
    let server = ArtifactServer::start(store, port).expect("failed to start artifact server");
    let url = server.base_url();
    eprintln!("agentflare artifacts server listening on {url}");
    eprintln!("  store: {}", dir.display());
    loop {
        std::thread::park();
    }
}
