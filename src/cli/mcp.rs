use clap::Args;

/// Run the agentflare MCP server (stdio by default; `--http` serves many
/// concurrent agent sessions from one persistent process over Streamable HTTP).
#[derive(Args)]
pub struct McpArgs {
    /// Serve over Streamable HTTP/SSE instead of stdio, so one agentflare
    /// process backs many concurrent agent sessions instead of one per session.
    #[arg(long)]
    pub http: bool,
    /// TCP port for `--http`. Default 35274 ("FLARE" shifted one up from the
    /// dashboard's 35273); 0 = auto-assign.
    #[arg(long, default_value = "35274")]
    pub port: u16,
    /// Interface to bind for `--http`. Loopback only for now: rmcp's default
    /// Host allowlist rejects remote clients, and there is no auth yet.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
}

impl McpArgs {
    pub fn run(self) {
        if self.http {
            let runtime = tokio::runtime::Runtime::new()
                .expect("failed to build tokio runtime for mcp server");
            if let Err(e) = runtime.block_on(crate::mcp_server::run_http(&self.host, self.port)) {
                crate::ui::error(&format!("agentflare mcp: {e}"));
                std::process::exit(1);
            }
            return;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime for mcp server");
        if let Err(e) = runtime.block_on(crate::mcp_server::run()) {
            crate::ui::error(&format!("agentflare mcp: {e}"));
            std::process::exit(1);
        }
    }
}
