use clap::{Args, Subcommand};

use crate::mcp_server::types::ProjectRequest;

#[derive(Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub command: ProjectCommands,
}

#[derive(Subcommand)]
pub enum ProjectCommands {
    /// Show the project this repo is linked to
    Info,
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
    /// Rename a project
    Rename {
        /// New project name
        name: String,
        /// Project name or UUID to rename (defaults to linked project)
        #[arg(long)]
        project: Option<String>,
        /// Optional new identifier prefix
        #[arg(long)]
        identifier: Option<String>,
    },
    /// Soft-delete a project
    Delete {
        /// Project name or UUID to delete
        project: String,
    },
}

impl ProjectArgs {
    pub fn run(self) {
        match self.command {
            ProjectCommands::Info => run_action(ProjectRequest {
                action: "info".into(),
                ..Default::default()
            }),
            ProjectCommands::Archive { project } => run_targeted("archive", project),
            ProjectCommands::Unarchive { project } => run_targeted("unarchive", project),
            ProjectCommands::Rename {
                name,
                project,
                identifier,
            } => run_action(ProjectRequest {
                action: "rename".into(),
                project,
                name: Some(name),
                identifier,
            }),
            ProjectCommands::Delete { project } => run_targeted("delete", project),
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

/// An action on one named project (`archive`/`unarchive`/`delete`).
fn run_targeted(action: &str, project: String) {
    run_action(ProjectRequest {
        action: action.into(),
        project: Some(project),
        ..Default::default()
    });
}

/// Every CLI project action goes through the MCP `project` tool's
/// implementation so the CLI and MCP surfaces can't drift (name-or-UUID
/// lookup, idempotency).
fn run_action(req: ProjectRequest) {
    match crate::mcp_server::AgentflareMcp::default().project_inner(req) {
        Ok(json) => println!("{json}"),
        Err(e) => crate::ui::error(&e.to_string()),
    }
}
