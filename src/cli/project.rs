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
    /// Park a project: the daemon stops dispatching its ready-for-work items
    /// and sweeping its PRs. It stays listed and its items stay readable.
    Archive {
        /// Project name or UUID (see `agentflare project list`)
        project: String,
    },
    /// Reverse `archive`. No re-registration of the checkout is needed.
    Unarchive {
        /// Project name or UUID (see `agentflare project list`)
        project: String,
    },
}

impl ProjectArgs {
    pub fn run(self) {
        match self.command {
            ProjectCommands::Archive { project } => set_archived("archive", project),
            ProjectCommands::Unarchive { project } => set_archived("unarchive", project),
            ProjectCommands::List { json: _ } => {
                let mcp = crate::mcp_server::AgentflareMcp::default();
                let res = mcp.with_backend_db(|conn| {
                    let workspace_id = crate::mcp_server::AgentflareMcp::resolve_workspace_id(conn)
                        .map_err(|e| rmcp::model::ErrorData::internal_error(e.to_string(), None))?;
                    let projects =
                        agentflare_backend::project::list_by_workspace(conn, &workspace_id)
                            .map_err(crate::mcp_server::types::map_backend_err)?;
                    let out: Vec<serde_json::Value> = projects
                        .into_iter()
                        .map(|p| {
                            serde_json::json!({
                                "id": p.id,
                                "name": p.name,
                                "identifier": p.identifier,
                                "workspace_id": p.workspace_id,
                                "archived_at": p.archived_at,
                            })
                        })
                        .collect();
                    Ok::<_, rmcp::model::ErrorData>(
                        serde_json::to_string_pretty(&out).unwrap_or_default(),
                    )
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

/// `archive` / `unarchive` share the MCP `project` tool's implementation so
/// the CLI and MCP surfaces can't drift (name-or-UUID lookup, idempotency).
fn set_archived(action: &str, project: String) {
    let mcp = crate::mcp_server::AgentflareMcp::default();
    match mcp.project_inner(crate::mcp_server::types::ProjectRequest {
        action: action.into(),
        project: Some(project),
    }) {
        Ok(json) => println!("{json}"),
        Err(e) => crate::ui::error(&e.to_string()),
    }
}
