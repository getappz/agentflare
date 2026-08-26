use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
pub struct InsightsArgs {
    #[command(subcommand)]
    pub command: Option<InsightsCommands>,
    /// Path to insights DB (default: ~/.local/share/agentflare/insights/observatory.db)
    #[arg(long)]
    pub db: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum InsightsCommands {
    /// Scan all agent sources and ingest into local DB
    Sync(SyncArgs),
    /// List sessions (recent first)
    List(ListArgs),
    /// Show one session with turns and tool calls
    Show(ShowArgs),
    /// Search sessions (FTS5 + trigram)
    Search(SearchArgs),
    /// Analytics: cost, tokens, heatmap
    Stats(StatsArgs),
    /// Export sessions (json/jsonl/html/deepeval/openai)
    Export(ExportArgs),
    /// Generate handoff doc to continue in another agent
    Handoff(HandoffArgs),
    /// Run local dashboard (127.0.0.1 only)
    Serve(ServeArgs),
}

#[derive(Parser, Debug)]
pub struct SyncArgs {
    #[arg(long, default_value = "0")]
    pub prune_days: u32,
}

#[derive(Parser, Debug)]
pub struct ListArgs {
    #[arg(long, default_value = "20")]
    pub limit: usize,
    #[arg(long, default_value = "0")]
    pub offset: usize,
    #[arg(long)]
    pub source: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Parser, Debug)]
pub struct ShowArgs {
    pub session_id: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Parser, Debug)]
pub struct SearchArgs {
    pub query: String,
    #[arg(long, default_value = "20")]
    pub limit: usize,
    #[arg(long)]
    pub json: bool,
}

#[derive(Parser, Debug)]
pub struct StatsArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Parser, Debug)]
pub struct ExportArgs {
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub source: Option<String>,
}

#[derive(Parser, Debug)]
pub struct HandoffArgs {
    pub session_id: String,
    #[arg(long, default_value = "codex")]
    pub target: String,
    #[arg(long, default_value = "standard")]
    pub verbosity: String,
}

#[derive(Parser, Debug)]
pub struct ServeArgs {
    #[arg(long, default_value = "3456")]
    pub port: u16,
}

impl InsightsArgs {
    pub fn run(self) {
        let db_path = self.db.unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".local/share/agentflare/insights/observatory.db")
        });
        let cmd = self
            .command
            .unwrap_or(InsightsCommands::List(ListArgs {
                limit: 20,
                offset: 0,
                source: None,
                json: false,
            }));
        match cmd {
            InsightsCommands::Sync(args) => run_sync(db_path, args),
            InsightsCommands::List(args) => run_list(db_path, args),
            InsightsCommands::Show(args) => run_show(db_path, args),
            InsightsCommands::Search(args) => run_search(db_path, args),
            InsightsCommands::Stats(args) => run_stats(db_path, args),
            InsightsCommands::Export(args) => run_export(db_path, args),
            InsightsCommands::Handoff(args) => run_handoff(db_path, args),
            InsightsCommands::Serve(args) => run_serve(db_path, args),
        }
    }
}

fn open_store(db: PathBuf) -> flare_insights::store::InsightsStore {
    flare_insights::store::InsightsStore::open(&db).unwrap_or_else(|e| {
        eprintln!("failed to open insights DB {}: {e}", db.display());
        std::process::exit(1);
    })
}

fn run_sync(db: PathBuf, args: SyncArgs) {
    let store = open_store(db.clone());
    let config = flare_insights::config::InsightsConfig::default();
    let mgr = flare_insights::ingest::IngestManager::new();
    let mut total = 0;
    for (source, res) in mgr.scan_all(&config) {
        match res {
            Ok(sessions) => {
                println!("{source}: {} sessions", sessions.len());
                for s in sessions {
                    let _ = store.upsert_session(&s);
                    total += 1;
                }
            }
            Err(e) => eprintln!("{source}: error {e}"),
        }
    }
    println!("synced {total} sessions -> {}", db.display());
    if args.prune_days > 0 {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(args.prune_days as i64);
        match store.prune_older_than(cutoff) {
            Ok(n) => println!("pruned {n} sessions older than {} days", args.prune_days),
            Err(e) => eprintln!("prune error: {e}"),
        }
    }
}

fn run_list(db: PathBuf, args: ListArgs) {
    let store = open_store(db);
    let sessions = store.list_sessions(args.limit, args.offset).unwrap_or_default();
    if args.json {
        println!("{}", serde_json::to_string_pretty(&sessions).unwrap());
    } else {
        for s in sessions {
            if let Some(filter) = &args.source {
                if s.source.as_str() != filter {
                    continue;
                }
            }
            let cost = s
                .cost
                .as_ref()
                .map(|c| format!("${:.2}", c.total_usd))
                .unwrap_or_else(|| "-".into());
            println!(
                "{:<36} {:<12} {:<16} {:>4} turns {:>4} tools {} {}",
                s.id,
                s.source.as_str(),
                s.project,
                s.turn_count,
                s.tool_call_count,
                cost,
                s.updated_at.format("%Y-%m-%d %H:%M")
            );
        }
    }
}

fn run_show(db: PathBuf, args: ShowArgs) {
    let store = open_store(db);
    match store.get_session(&args.session_id) {
        Ok(Some(s)) => {
            if args.json {
                println!("{}", serde_json::to_string_pretty(&s).unwrap());
            } else {
                println!("{} [{}] {}", s.id, s.source.as_str(), s.project);
                println!(
                    "model: {} status: {:?} updated: {}",
                    s.model.as_deref().unwrap_or("-"),
                    s.status,
                    s.updated_at
                );
                println!(
                    "tokens: in={} out={} cache_read={} cache_write={} total={}",
                    s.tokens.input,
                    s.tokens.output,
                    s.tokens.cache_read,
                    s.tokens.cache_write,
                    s.tokens.total()
                );
                if let Some(c) = s.cost {
                    println!("cost: ${:.4}", c.total_usd);
                }
                println!(
                    "turns: {} tools: {} subagents: {}",
                    s.turn_count, s.tool_call_count, s.subagent_count
                );
            }
        }
        Ok(None) => {
            eprintln!("session not found: {}", args.session_id);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn run_search(db: PathBuf, args: SearchArgs) {
    let store = open_store(db);
    let opts = flare_insights::search::SearchOptions {
        query: args.query.clone(),
        source: None,
        project: None,
        limit: args.limit,
        offset: 0,
    };
    let res = flare_insights::search::search(&store, &opts).unwrap_or_default();
    if args.json {
        println!("{}", serde_json::to_string_pretty(&res).unwrap());
    } else {
        for s in res {
            println!(
                "{:<36} {:<12} {} {}",
                s.id,
                s.source.as_str(),
                s.project,
                s.title.as_deref().unwrap_or("")
            )
        }
    }
}

fn run_stats(db: PathBuf, args: StatsArgs) {
    let store = open_store(db);
    let sessions = store.list_sessions(10000, 0).unwrap_or_default();
    let analytics = flare_insights::analytics::compute_analytics(&sessions);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&analytics).unwrap());
    } else {
        println!(
            "sessions: {} tokens: {} cost: ${:.2} cache_hit: {:.1}%",
            analytics.total_sessions,
            analytics.total_tokens,
            analytics.total_cost_usd,
            analytics.cache_hit_rate * 100.0
        );
        println!("by_source: {:?}", analytics.by_source);
        println!("by_project: {:?}", analytics.by_project);
        for b in &analytics.by_day {
            println!("{}: {} sessions ${:.2}", b.date, b.sessions, b.cost_usd);
        }
    }
}

fn run_export(db: PathBuf, args: ExportArgs) {
    let store = open_store(db);
    let sessions = store.list_sessions(10000, 0).unwrap_or_default();
    let fmt = match args.format.as_str() {
        "jsonl" => flare_insights::export::ExportFormat::Jsonl,
        "html" => flare_insights::export::ExportFormat::Html,
        "deepeval" => flare_insights::export::ExportFormat::Deepeval,
        "openai" | "openai-evals" => flare_insights::export::ExportFormat::OpenAiEvals,
        _ => flare_insights::export::ExportFormat::Json,
    };
    let out = flare_insights::export::export_sessions(&sessions, &[], fmt);
    if let Some(path) = args.output {
        std::fs::write(&path, out).unwrap();
        println!("exported to {}", path.display());
    } else {
        println!("{out}");
    }
}

fn run_handoff(db: PathBuf, args: HandoffArgs) {
    let store = open_store(db);
    let Some(session) = store.get_session(&args.session_id).unwrap_or(None) else {
        eprintln!("session not found");
        std::process::exit(1);
    };
    let verbosity = match args.verbosity.as_str() {
        "minimal" => flare_insights::handoff::Verbosity::Minimal,
        "verbose" => flare_insights::handoff::Verbosity::Verbose,
        "full" => flare_insights::handoff::Verbosity::Full,
        _ => flare_insights::handoff::Verbosity::Standard,
    };
    let doc = flare_insights::handoff::handoff_doc(&session, &[], &args.target, verbosity);
    println!("{doc}");
}

fn run_serve(_db: PathBuf, args: ServeArgs) {
    println!("flare-insights serve on 127.0.0.1:{} (API + WS)", args.port);
    println!("endpoints: GET /api/sessions  GET /api/search?q=  GET /api/stats  WS /ws");
    println!("(full axum server behind `api` feature — scaffold ready, bind 127.0.0.1 only)");
}
