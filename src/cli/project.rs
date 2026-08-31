use clap::{Args, Subcommand};

#[derive(Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub command: ProjectCommands,
}

#[derive(Subcommand)]
pub enum ProjectCommands {
    /// List projects in workspace (JSON)
    List {
        #[arg(long, default_value_t = true)]
        json: bool,
    },
}

impl ProjectArgs {
    pub fn run(self) {
        match self.command {
            ProjectCommands::List { json: _ } => {
                let mcp = crate::mcp_server::AgentflareMcp::default();
                let res = mcp.with_backend_db(|conn| {
                    let workspace_id = crate::mcp_server::AgentflareMcp::resolve_workspace_id(conn).map_err(|e| rmcp::model::ErrorData::internal_error(e.to_string(), None))?;
                    let projects = agentflare_backend::project::list_by_workspace(conn, &workspace_id).map_err(crate::mcp_server::types::map_backend_err)?;
                    let out: Vec<serde_json::Value> = projects
                        .into_iter()
                        .map(|p| {
                            serde_json::json!({
                                "id": p.id,
                                "name": p.name,
                                "identifier": p.identifier,
                                "workspace_id": p.workspace_id,
                            })
                        })
                        .collect();
                    Ok::<_, rmcp::model::ErrorData>(serde_json::to_string_pretty(&out).unwrap_or_default())
                });
                match res {
                    Ok(Ok(json)) => println!("{json}"),
                    Ok(Err(e)) => crate::ui::error(&e.to_string()),
                    Err(e) => crate::ui::error(&e.to_string()),
                }
            }
        }
    }
}
