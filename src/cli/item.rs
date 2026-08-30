use clap::{Args, Subcommand};

#[derive(Args)]
pub struct ItemArgs {
    #[command(subcommand)]
    pub command: ItemCommands,
}

#[derive(Subcommand)]
pub enum ItemCommands {
    /// List work items for current project (JSON)
    List {
        /// Output JSON (default)
        #[arg(long, default_value_t = true)]
        json: bool,
        /// Limit
        #[arg(long)]
        limit: Option<i64>,
        /// Filter by state group (e.g. backlog,unstarted,started,in_review,completed)
        #[arg(long)]
        state_group: Option<String>,
    },
}

impl ItemArgs {
    pub fn run(self) {
        match self.command {
            ItemCommands::List { json: _, limit, state_group } => {
                let mcp = crate::mcp_server::AgentflareMcp::default();
                let res = mcp.with_backend_db(|conn| {
                    let project = mcp.resolve_project(conn)?;
                    let mut items = agentflare_backend::item::list_by_project(conn, &project.id).map_err(crate::mcp_server::types::map_backend_err)?;
                    let states = agentflare_backend::state::list_by_project(conn, &project.id).map_err(crate::mcp_server::types::map_backend_err)?;
                    let state_by_id: std::collections::HashMap<&str, &agentflare_backend::state::State> =
                        states.iter().map(|s| (s.id.as_str(), s)).collect();

                    if let Some(group) = &state_group {
                        let wanted: Vec<&str> = group.split(',').map(str::trim).collect();
                        items.retain(|i| {
                            state_by_id
                                .get(i.state_id.as_str())
                                .map(|s| wanted.contains(&s.group_name.as_str()))
                                .unwrap_or(false)
                        });
                    }

                    let lim = limit.unwrap_or(50).clamp(0, 500) as usize;
                    let page: Vec<_> = items.into_iter().take(lim).collect();

                    let out: Vec<serde_json::Value> = page
                        .into_iter()
                        .map(|i| {
                            let state = state_by_id.get(i.state_id.as_str());
                            let state_name = state.map(|s| s.name.clone()).unwrap_or_default();
                            let state_group = state.map(|s| s.group_name.clone()).unwrap_or_default();
                            serde_json::json!({
                                "id": format!("#{}", i.sequence_id),
                                "name": i.name,
                                "state": state_name,
                                "state_group": state_group,
                                "priority": i.priority,
                                "assignee": i.assignee_agent,
                                "labels": [],
                                "sequence_id": i.sequence_id,
                                "updated_at": i.updated_at,
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
