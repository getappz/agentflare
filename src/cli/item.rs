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
        #[arg(long, default_value_t = true)]
        json: bool,
        #[arg(long)]
        limit: Option<i64>,
        #[arg(long)]
        state_group: Option<String>,
    },
    /// Update item state (e.g. --state "Started")
    UpdateState {
        #[arg(long)]
        id: String,
        #[arg(long)]
        state: String,
    },
    /// Create a new work item
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        priority: Option<String>,
        #[arg(long)]
        state: Option<String>,
    },
    /// Get a single item by id (#1 or UUID)
    Get {
        #[arg(long)]
        id: String,
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
            ItemCommands::UpdateState { id, state } => {
                let mcp = crate::mcp_server::AgentflareMcp::default();
                let res = mcp.with_backend_db(|conn| {
                    let item_id = mcp.resolve_item_id(conn, &id)?;
                    let project = mcp.resolve_project(conn)?;
                    let states = agentflare_backend::state::list_by_project(conn, &project.id).map_err(crate::mcp_server::types::map_backend_err)?;
                    let target = states.iter().find(|s| s.name.to_lowercase() == state.to_lowercase() || s.group_name.to_lowercase() == state.to_lowercase().replace(' ', "_")).cloned().ok_or_else(|| rmcp::model::ErrorData::invalid_params(format!("state '{}' not found", state), None))?;
                    let item = agentflare_backend::item::update_state(conn, &item_id, &target.id).map_err(crate::mcp_server::types::map_backend_err)?;
                    Ok::<_, rmcp::model::ErrorData>(serde_json::to_string_pretty(&item).unwrap_or_default())
                });
                match res {
                    Ok(Ok(json)) => println!("{json}"),
                    Ok(Err(e)) => crate::ui::error(&e.to_string()),
                    Err(e) => crate::ui::error(&e.to_string()),
                }
            }
            ItemCommands::Create { name, description, priority, state } => {
                let mcp = crate::mcp_server::AgentflareMcp::default();
                let res = mcp.with_backend_db(|conn| {
                    let project = mcp.resolve_project(conn)?;
                    let states = agentflare_backend::state::list_by_project(conn, &project.id).map_err(crate::mcp_server::types::map_backend_err)?;
                    let state_id = if let Some(s) = state {
                        states.iter().find(|st| st.name.to_lowercase() == s.to_lowercase() || st.group_name.to_lowercase() == s.to_lowercase().replace(' ', "_")).map(|st| st.id.clone()).unwrap_or_else(|| states.iter().find(|st| st.is_default).map(|st| st.id.clone()).unwrap_or_else(|| states[0].id.clone()))
                    } else {
                        states.iter().find(|st| st.is_default).map(|st| st.id.clone()).unwrap_or_else(|| states[0].id.clone())
                    };
                    let item = agentflare_backend::item::create(
                        conn,
                        agentflare_backend::item::CreateItem {
                            project_id: project.id,
                            state_id,
                            name: name.clone(),
                            description,
                            priority,
                            parent_id: None,
                            assignee_agent: None,
                            sort_order: None,
                            external_source: None,
                            external_id: None,
                            metadata: None,
                            label_ids: vec![],
                            assignee_ids: vec![],
                            dependency_ids: vec![],
                        },
                    )
                    .map_err(crate::mcp_server::types::map_backend_err)?;
                    Ok::<_, rmcp::model::ErrorData>(serde_json::to_string_pretty(&item).unwrap_or_default())
                });
                match res {
                    Ok(Ok(json)) => println!("{json}"),
                    Ok(Err(e)) => crate::ui::error(&e.to_string()),
                    Err(e) => crate::ui::error(&e.to_string()),
                }
            }
            ItemCommands::Get { id } => {
                let mcp = crate::mcp_server::AgentflareMcp::default();
                let res = mcp.with_backend_db(|conn| {
                    let item_id = mcp.resolve_item_id(conn, &id)?;
                    let item = agentflare_backend::item::get(conn, &item_id).map_err(crate::mcp_server::types::map_backend_err)?;
                    let state = agentflare_backend::state::get(conn, &item.state_id).map_err(crate::mcp_server::types::map_backend_err)?;
                    let out = serde_json::json!({
                        "id": format!("#{}", item.sequence_id),
                        "uuid": item.id,
                        "name": item.name,
                        "description": item.description,
                        "state": state.name,
                        "state_group": state.group_name,
                        "priority": item.priority,
                        "assignee": item.assignee_agent,
                        "sequence_id": item.sequence_id,
                        "created_at": item.created_at,
                        "updated_at": item.updated_at,
                    });
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
