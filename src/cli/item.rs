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
        #[arg(long)]
        project: Option<String>,
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
    /// Approve an item's pending plan (human-only; refuses under an AI agent)
    ApprovePlan {
        /// Item id (#1 or UUID)
        id: String,
    },
    /// Cancel an item for good: kills its running agent, cancels its queued
    /// jobs and workflow run, and moves it to the cancelled state
    Cancel {
        /// Item id (#1 or UUID)
        id: String,
        /// Why (posted as a comment on the item)
        #[arg(long)]
        reason: Option<String>,
    },
    /// Pause an item's run at its next step boundary (worktree and run state
    /// are kept; the running agent is stopped)
    Pause {
        /// Item id (#1 or UUID)
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Resume a paused item: the next dispatch continues its run from the
    /// same step
    Resume {
        /// Item id (#1 or UUID)
        id: String,
    },
    /// Re-arm an item for dispatch, optionally to a different agent (the
    /// previous agent's jobs are cancelled and its claim released)
    Redispatch {
        /// Item id (#1 or UUID)
        id: String,
        /// Agent to hand the item to (e.g. claude-code, codex)
        #[arg(long)]
        agent: Option<String>,
    },
}

/// Runs one operator control through `job_controls::item_action` and prints
/// its JSON reply, exiting non-zero on error.
pub(crate) fn run_item_control(
    id: &str,
    action: &str,
    reason: Option<String>,
    agent: Option<String>,
) {
    match crate::job_controls::item_action(id, action, reason, agent) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            crate::ui::error(&e);
            std::process::exit(1);
        }
    }
}

/// `Some(message)` when the caller is an AI agent. `approve-plan` is the CLI
/// twin of a human's Telegram tap, so it must stay out of an agent's reach —
/// same detection `agentflare work` uses.
fn approve_plan_denial(agent: Option<&str>) -> Option<String> {
    agent.map(|agent| {
        format!(
            "`agentflare item approve-plan` is a human-only command — it stands in for a \
             human's approval on the Telegram card, so an agent running it would defeat the \
             plan gate (detected this process is running under the {agent} AI agent). Ask a \
             human to run it."
        )
    })
}

impl ItemArgs {
    pub fn run(self) {
        match self.command {
            ItemCommands::Cancel { id, reason } => run_item_control(&id, "cancel", reason, None),
            ItemCommands::Pause { id, reason } => run_item_control(&id, "pause", reason, None),
            ItemCommands::Resume { id } => run_item_control(&id, "resume", None, None),
            ItemCommands::Redispatch { id, agent } => {
                run_item_control(&id, "redispatch", None, agent)
            }
            ItemCommands::ApprovePlan { id } => {
                if let Some(msg) = approve_plan_denial(agent_detector::agent_name().as_deref()) {
                    eprintln!("error: {msg}");
                    std::process::exit(1);
                }
                let mcp = crate::mcp_server::AgentflareMcp::default();
                match mcp.item_approve_plan_via_channel(crate::mcp_server::types::ItemRequest {
                    action: "approve_plan".into(),
                    id: Some(id),
                    ..Default::default()
                }) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        crate::ui::error(&e.to_string());
                        std::process::exit(1);
                    }
                }
            }
            ItemCommands::List {
                json: _,
                limit,
                state_group,
                project: project_filter,
            } => {
                let mcp = crate::mcp_server::AgentflareMcp::default();
                let res = mcp.with_backend_db(|conn| {
                    let project = if let Some(p) = project_filter {
                        mcp.resolve_project_for_read(conn, Some(&p))?
                    } else {
                        mcp.resolve_project(conn)?
                    };
                    let mut items = agentflare_backend::item::list_by_project(conn, &project.id)
                        .map_err(crate::mcp_server::types::map_backend_err)?;
                    let states = agentflare_backend::state::list_by_project(conn, &project.id)
                        .map_err(crate::mcp_server::types::map_backend_err)?;
                    let state_by_id: std::collections::HashMap<
                        &str,
                        &agentflare_backend::state::State,
                    > = states.iter().map(|s| (s.id.as_str(), s)).collect();

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
                            let state_group =
                                state.map(|s| s.group_name.clone()).unwrap_or_default();
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
            ItemCommands::UpdateState { id, state } => {
                let mcp = crate::mcp_server::AgentflareMcp::default();
                let res = mcp.with_backend_db(|conn| {
                    let item_id = mcp.resolve_item_id(conn, &id)?;
                    let project = mcp.resolve_project(conn)?;
                    let states = agentflare_backend::state::list_by_project(conn, &project.id)
                        .map_err(crate::mcp_server::types::map_backend_err)?;
                    let target = states
                        .iter()
                        .find(|s| {
                            s.name.to_lowercase() == state.to_lowercase()
                                || s.group_name.to_lowercase()
                                    == state.to_lowercase().replace(' ', "_")
                        })
                        .cloned()
                        .ok_or_else(|| {
                            rmcp::model::ErrorData::invalid_params(
                                format!("state '{}' not found", state),
                                None,
                            )
                        })?;
                    let item = agentflare_backend::item::update_state(conn, &item_id, &target.id)
                        .map_err(crate::mcp_server::types::map_backend_err)?;
                    Ok::<_, rmcp::model::ErrorData>(
                        serde_json::to_string_pretty(&item).unwrap_or_default(),
                    )
                });
                match res {
                    Ok(Ok(json)) => println!("{json}"),
                    Ok(Err(e)) => crate::ui::error(&e.to_string()),
                    Err(e) => crate::ui::error(&e.to_string()),
                }
            }
            ItemCommands::Create {
                name,
                description,
                priority,
                state,
            } => {
                let mcp = crate::mcp_server::AgentflareMcp::default();
                let res = mcp.with_backend_db(|conn| {
                    let project = mcp.resolve_project(conn)?;
                    let states = agentflare_backend::state::list_by_project(conn, &project.id)
                        .map_err(crate::mcp_server::types::map_backend_err)?;
                    let state_id = if let Some(s) = state {
                        states
                            .iter()
                            .find(|st| {
                                st.name.to_lowercase() == s.to_lowercase()
                                    || st.group_name.to_lowercase()
                                        == s.to_lowercase().replace(' ', "_")
                            })
                            .map(|st| st.id.clone())
                            .unwrap_or_else(|| {
                                states
                                    .iter()
                                    .find(|st| st.is_default)
                                    .map(|st| st.id.clone())
                                    .unwrap_or_else(|| states[0].id.clone())
                            })
                    } else {
                        states
                            .iter()
                            .find(|st| st.is_default)
                            .map(|st| st.id.clone())
                            .unwrap_or_else(|| states[0].id.clone())
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
                            start_date: None,
                            due_date: None,
                        },
                    )
                    .map_err(crate::mcp_server::types::map_backend_err)?;
                    Ok::<_, rmcp::model::ErrorData>(
                        serde_json::to_string_pretty(&item).unwrap_or_default(),
                    )
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
                    let item = agentflare_backend::item::get(conn, &item_id)
                        .map_err(crate::mcp_server::types::map_backend_err)?;
                    let state = agentflare_backend::state::get(conn, &item.state_id)
                        .map_err(crate::mcp_server::types::map_backend_err)?;
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

#[cfg(test)]
mod tests {
    use super::approve_plan_denial;

    #[test]
    fn approve_plan_is_denied_under_an_ai_agent() {
        let msg = approve_plan_denial(Some("claude-code")).expect("agent must be denied");
        assert!(
            msg.contains("human-only") && msg.contains("claude-code"),
            "{msg}"
        );
    }

    #[test]
    fn approve_plan_is_allowed_for_a_human() {
        assert_eq!(approve_plan_denial(None), None);
    }
}
