use clap::Args;

#[derive(Args)]
pub struct McpArgs;

impl McpArgs {
    pub fn run(self) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime for mcp server");
        if let Err(e) = runtime.block_on(crate::mcp_server::run()) {
            eprintln!("agentflare mcp: {e}");
            std::process::exit(1);
        }
    }
}
