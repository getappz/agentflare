use clap::{Args, Subcommand};

/// Query and manage agentflare's long-term memory (sessions, observations, embeddings).
#[derive(Args)]
pub struct MemoryArgs {
    #[command(subcommand)]
    pub command: MemoryCommands,
}

#[derive(Subcommand)]
pub enum MemoryCommands {
    /// Show active session context (recent sessions, observations, summaries)
    Context {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Search observations with FTS5
    Search {
        query: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value = "10")]
        limit: usize,
    },
    /// List recent sessions
    Sessions {
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value = "10")]
        limit: usize,
    },
    /// List recent observations
    Observations {
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value = "10")]
        limit: usize,
    },
    /// Save an observation. Write a retrieval card: error text/symptom + where it
    /// shows up (path/identifier) + fix location. Vague text is saved but the
    /// output carries `warnings` with a rewrite hint.
    Remember {
        title: String,
        content: String,
        #[arg(long, default_value = "note")]
        r#type: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        topic_key: Option<String>,
        #[arg(long)]
        scope: Option<String>,
    },
    /// Compute embeddings for observations missing them.
    /// Requires a build with --features semantic and a downloaded model.
    BackfillEmbeddings {
        #[arg(long, default_value = "200")]
        batch: usize,
    },
    /// Sync observations with a shared GitHub branch so other workstations
    /// see the same facts. Defaults to this repo's `origin` remote; set
    /// AGENTFLARE_MEMORY_SYNC_REPO=owner/repo to point elsewhere
    /// (AGENTFLARE_MEMORY_SYNC_BRANCH/_PATH override the branch/file name).
    Sync,
}

impl MemoryArgs {
    pub fn run(self) {
        match self.command {
            MemoryCommands::Context {
                project,
                session_id,
            } => {
                let input = crate::memory::mcp::ContextInput {
                    session_id,
                    project,
                };
                match crate::memory::mcp::handle_context(input) {
                    Ok(out) => println!("{out}"),
                    Err(e) => crate::ui::error(&e.to_string()),
                }
            }
            MemoryCommands::Search {
                query,
                project,
                limit,
            } => {
                let input = crate::memory::mcp::RecallInput {
                    query,
                    id: None,
                    r#type: None,
                    project,
                    limit: Some(limit),
                };
                match crate::memory::mcp::handle_recall(input) {
                    Ok(out) => println!("{out}"),
                    Err(e) => crate::ui::error(&e.to_string()),
                }
            }
            MemoryCommands::Remember {
                title,
                content,
                r#type,
                project,
                topic_key,
                scope,
            } => {
                let input = crate::memory::mcp::RememberInput {
                    title,
                    content,
                    r#type,
                    session_id: None,
                    project,
                    topic_key,
                    scope,
                };
                match crate::memory::mcp::handle_remember(input) {
                    Ok(out) => println!("{out}"),
                    Err(e) => {
                        crate::ui::error(&e);
                        std::process::exit(1);
                    }
                }
            }
            MemoryCommands::Sessions { project, limit } => match crate::memory::store::open() {
                Err(e) => crate::ui::error(&e.to_string()),
                Ok(conn) => {
                    match crate::memory::sessions::list_recent(&conn, project.as_deref(), limit) {
                        Ok(sessions) => println!(
                            "{}",
                            serde_json::to_string_pretty(&sessions).unwrap_or_default()
                        ),
                        Err(e) => crate::ui::error(&e.to_string()),
                    }
                }
            },
            MemoryCommands::Observations { project, limit } => match crate::memory::store::open() {
                Err(e) => crate::ui::error(&e.to_string()),
                Ok(conn) => match crate::memory::observations::list_recent(
                    &conn,
                    project.as_deref(),
                    None,
                    limit,
                ) {
                    Ok(obs) => {
                        let views = obs
                            .iter()
                            .map(|o| crate::memory::mcp::observation_view(&conn, o))
                            .collect::<rusqlite::Result<Vec<_>>>();
                        match views {
                            Ok(views) => println!(
                                "{}",
                                serde_json::to_string_pretty(&views).unwrap_or_default()
                            ),
                            Err(e) => crate::ui::error(&e.to_string()),
                        }
                    }
                    Err(e) => crate::ui::error(&e.to_string()),
                },
            },
            MemoryCommands::BackfillEmbeddings { batch } => {
                let Some(model) = crate::memory::engine::model_name() else {
                    crate::ui::error(
                        "embedding engine unavailable — build with --features semantic and ensure the model is downloaded",
                    );
                    return;
                };
                match crate::memory::store::open() {
                    Err(e) => crate::ui::error(&e.to_string()),
                    Ok(conn) => match crate::memory::embeddings::missing(&conn, batch) {
                        Err(e) => crate::ui::error(&e.to_string()),
                        Ok(todo) => {
                            let total = todo.len();
                            let mut ok = 0usize;
                            for (id, text) in todo {
                                match crate::memory::engine::embed_doc(&text) {
                                    Some(vec) => {
                                        match crate::memory::embeddings::upsert(
                                            &conn, id, &vec, &model,
                                        ) {
                                            Ok(()) => ok += 1,
                                            Err(e) => crate::ui::warning(&format!(
                                                "obs {id}: store failed: {e}"
                                            )),
                                        }
                                    }
                                    None => crate::ui::warning(&format!("obs {id}: embed failed")),
                                }
                            }
                            crate::ui::success(&format!(
                                "backfilled {ok}/{total} embeddings (model: {model})"
                            ));
                        }
                    },
                }
            }
            MemoryCommands::Sync => run_sync(),
        }
    }
}

fn run_sync() {
    let config = match crate::memory::sync::MemorySyncConfig::from_env() {
        Ok(c) => c,
        Err(e) => return crate::ui::error(&e),
    };
    let client = match crate::github::Client::new() {
        Ok(c) => c,
        Err(e) => return crate::ui::error(&e.to_string()),
    };
    let conn = match crate::memory::store::open() {
        Ok(c) => c,
        Err(e) => return crate::ui::error(&e.to_string()),
    };
    match crate::memory::sync::run(&conn, &client, &config) {
        Ok(report) => println!(
            "synced with {}@{}: {} observation(s) changed locally, remote {}",
            config.repo,
            config.branch,
            report.imported,
            if report.pushed {
                "updated"
            } else {
                "already current"
            }
        ),
        Err(e) => crate::ui::error(&e),
    }
}
