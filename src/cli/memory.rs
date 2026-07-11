use clap::{Args, Subcommand};

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
}

impl MemoryArgs {
    pub fn run(self) {
        match self.command {
            MemoryCommands::Context { project, session_id } => {
                let input = crate::memory::mcp::ContextInput {
                    session_id,
                    project,
                };
                match crate::memory::mcp::handle_context(input) {
                    Ok(out) => println!("{out}"),
                    Err(e) => eprintln!("error: {e}"),
                }
            }
            MemoryCommands::Search { query, project, limit } => {
                let input = crate::memory::mcp::RecallInput {
                    query,
                    id: None,
                    r#type: None,
                    project,
                    limit: Some(limit),
                };
                match crate::memory::mcp::handle_recall(input) {
                    Ok(out) => println!("{out}"),
                    Err(e) => eprintln!("error: {e}"),
                }
            }
            MemoryCommands::Sessions { project, limit } => {
                match crate::memory::store::open() {
                    Err(e) => eprintln!("error: {e}"),
                    Ok(conn) => match crate::memory::sessions::list_recent(&conn, project.as_deref(), limit) {
                        Ok(sessions) => println!("{}", serde_json::to_string_pretty(&sessions).unwrap_or_default()),
                        Err(e) => eprintln!("error: {e}"),
                    },
                }
            }
            MemoryCommands::Observations { project, limit } => {
                match crate::memory::store::open() {
                    Err(e) => eprintln!("error: {e}"),
                    Ok(conn) => match crate::memory::observations::list_recent(&conn, project.as_deref(), None, limit) {
                        Ok(obs) => println!("{}", serde_json::to_string_pretty(&obs).unwrap_or_default()),
                        Err(e) => eprintln!("error: {e}"),
                    },
                }
            }
        }
    }
}
