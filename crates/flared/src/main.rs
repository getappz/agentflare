use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use flared::config::Config;
use flared::daemon::{make_verified_killer, sweep_once, unix_now};
use flared::events::EventLog;
use flared::http::AppState;
use flared::janitor::lean_ctx::{check_registry, prune_registry};
use flared::leases::{default_state_dir, LeaseStore};
use flared::model::Identity;
use flared::scanner::{identity_of, scan};

#[derive(Parser)]
#[command(name = "flared", version, about = "Always-on supervisor for AI-agent workload hygiene")]
struct Cli {
    /// Path to config.toml (default: ~/.config/flared/config.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Machine-readable JSON output where supported
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// One-shot pressure + workload summary
    Status,
    /// Hottest processes by memory
    Ps {
        #[arg(long, default_value_t = 15)]
        top: usize,
    },
    /// Plan cleanup; dry run unless --execute
    Clean {
        /// Actually kill Safe, identity-verified expired leases
        #[arg(long)]
        execute: bool,
    },
    /// Heuristic orphan findings (report-only)
    Orphans,
    /// Check configured state registries for stale entries
    RegistryCheck {
        /// Prune stale entries (backup written first)
        #[arg(long)]
        execute: bool,
    },
    /// Manage leases
    Lease {
        #[command(subcommand)]
        command: LeaseCommand,
    },
    /// Run the always-on supervisor (sweep loop + HTTP on 127.0.0.1)
    Serve {
        #[arg(long)]
        port: Option<u16>,
    },
    /// Autostart recipes
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
}

#[derive(Subcommand)]
enum LeaseCommand {
    /// Register a lease for a live pid
    Add {
        #[arg(long)]
        pid: u32,
        #[arg(long, default_value = "agent")]
        class: String,
        /// TTL in seconds
        #[arg(long)]
        ttl: u64,
        /// Authorize flared to kill the tree when the lease expires
        #[arg(long)]
        allow_kill: bool,
    },
    List,
    /// Reset a lease's clock
    Heartbeat { id: String },
    Remove { id: String },
}

#[derive(Subcommand)]
enum ServiceCommand {
    /// Print the autostart recipe for this platform
    Print,
}

fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();
    let cfg = Config::load(cli.config.as_deref());
    let state_dir = default_state_dir();
    let store = LeaseStore::new(&state_dir);
    let log = EventLog::new(&state_dir);

    match cli.command {
        Command::Status => {
            let outcome = sweep_once(&cfg, &store, &log, false, true, &mut make_verified_killer(&store))?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            } else {
                let p = &outcome.pressure;
                println!("pressure: {}", p.level);
                println!(
                    "  cpu {:.0}%  mem free {:.1}/{:.1} GiB  swap {:.1}/{:.1} GiB",
                    p.cpu_pct,
                    gib(p.avail_mem_bytes),
                    gib(p.total_mem_bytes),
                    gib(p.swap_used_bytes),
                    gib(p.swap_total_bytes),
                );
                let mut buckets: Vec<_> = outcome.bucket_counts.iter().collect();
                buckets.sort_by(|a, b| b.1.cmp(a.1));
                for (bucket, count) in buckets {
                    println!("  {bucket:<10} {count}");
                }
                println!("leases: {}  orphan findings: {}", outcome.lease_count, outcome.orphans.len());
                for reg in &outcome.registries {
                    println!(
                        "registry {}: {} entries, {} live, {} stale",
                        reg.path, reg.report.total, reg.report.live, reg.report.stale.len()
                    );
                }
            }
        }
        Command::Ps { top } => {
            let (procs, _) = scan(&cfg);
            let mut list: Vec<_> = procs.into_values().collect();
            list.sort_by_key(|p| std::cmp::Reverse(p.rss_bytes));
            println!("{:>8} {:>8} {:>7} {:<10} name", "pid", "rss MiB", "cpu%", "bucket");
            for p in list.into_iter().take(top) {
                println!(
                    "{:>8} {:>8.0} {:>7.1} {:<10} {}{}",
                    p.pid,
                    p.rss_bytes as f64 / (1 << 20) as f64,
                    p.cpu_pct,
                    format!("{:?}", p.bucket).to_ascii_lowercase(),
                    p.name,
                    if p.protected { " [protected]" } else { "" },
                );
            }
        }
        Command::Clean { execute } => {
            let outcome = sweep_once(&cfg, &store, &log, execute, true, &mut make_verified_killer(&store))?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&outcome.outcomes)?);
            } else if outcome.actions.is_empty() {
                println!("nothing to clean: no expired leases");
            } else {
                for o in &outcome.outcomes {
                    println!(
                        "[{}] {} — {} ({})",
                        if o.executed { "KILLED" } else { "planned" },
                        o.action.target,
                        o.action.reason,
                        o.detail,
                    );
                }
                if !execute {
                    println!("dry run — pass --execute to act on Safe actions");
                }
            }
        }
        Command::Orphans => {
            let outcome = sweep_once(&cfg, &store, &log, false, true, &mut make_verified_killer(&store))?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&outcome.orphans)?);
            } else if outcome.orphans.is_empty() {
                println!("no orphan findings");
            } else {
                for f in &outcome.orphans {
                    println!("{:>8} {} — {}", f.pid, f.name, f.reason);
                }
                println!("(report-only; nothing was killed)");
            }
        }
        Command::RegistryCheck { execute } => {
            let (procs, _) = scan(&cfg);
            let mut results = Vec::new();
            for reg in &cfg.registries {
                // Same filter as the daemon: only formats we understand may
                // ever be parsed, let alone pruned.
                if reg.kind != "lean-ctx" {
                    if !cli.json {
                        println!("skip {} (unsupported kind '{}')", reg.path.display(), reg.kind);
                    }
                    results.push(serde_json::json!({
                        "path": reg.path.display().to_string(),
                        "skipped": format!("unsupported kind '{}'", reg.kind),
                    }));
                    continue;
                }
                if !reg.path.exists() {
                    if !cli.json {
                        println!("skip {} (missing)", reg.path.display());
                    }
                    results.push(serde_json::json!({
                        "path": reg.path.display().to_string(),
                        "skipped": "missing",
                    }));
                    continue;
                }
                let report =
                    check_registry(&reg.path, &procs, &reg.expected_exe, cfg.identity_tolerance_secs)?;
                if !cli.json {
                    println!(
                        "{}: {} entries, {} live, {} stale",
                        reg.path.display(),
                        report.total,
                        report.live,
                        report.stale.len()
                    );
                    for s in report.stale.iter().take(10) {
                        println!("  stale {} (pid {}): {}", s.agent_id, s.pid, s.reason);
                    }
                    if report.stale.len() > 10 {
                        println!("  … and {} more", report.stale.len() - 10);
                    }
                }
                let mut backup_path = None;
                if execute && !report.stale.is_empty() {
                    let backup = prune_registry(&reg.path, &report)?;
                    log.append(
                        "registry.prune",
                        serde_json::json!({
                            "path": reg.path.display().to_string(),
                            "removed": report.stale.len(),
                            "backup": backup.display().to_string(),
                        }),
                    )?;
                    if !cli.json {
                        println!(
                            "pruned {} stale entries (backup: {})",
                            report.stale.len(),
                            backup.display()
                        );
                    }
                    backup_path = Some(backup.display().to_string());
                }
                results.push(serde_json::json!({
                    "path": reg.path.display().to_string(),
                    "report": report,
                    "pruned": execute && backup_path.is_some(),
                    "backup": backup_path,
                }));
            }
            if cli.json {
                // One valid JSON document, whatever the number of registries.
                println!("{}", serde_json::to_string_pretty(&results)?);
            }
        }
        Command::Lease { command } => match command {
            LeaseCommand::Add { pid, class, ttl, allow_kill } => {
                let Some((exe_name, start_time)) = identity_of(pid) else {
                    eyre::bail!("pid {pid} is not running");
                };
                let lease = store.create(
                    pid,
                    &class,
                    ttl,
                    Identity { exe_name, start_time },
                    allow_kill,
                    unix_now(),
                )?;
                println!("{}", serde_json::to_string_pretty(&lease)?);
            }
            LeaseCommand::List => {
                println!("{}", serde_json::to_string_pretty(&store.load()?)?);
            }
            LeaseCommand::Heartbeat { id } => match store.heartbeat(&id, unix_now())? {
                Some(lease) => println!("{}", serde_json::to_string_pretty(&lease)?),
                None => eyre::bail!("unknown lease '{id}'"),
            },
            LeaseCommand::Remove { id } => {
                println!("removed: {}", store.remove(&id)?);
            }
        },
        Command::Serve { port } => {
            let mut cfg = cfg;
            if let Some(port) = port {
                cfg.port = port;
            }
            let cfg = Arc::new(cfg);
            let state = AppState::new(Arc::clone(&cfg), state_dir);
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(async {
                let loop_handle = tokio::spawn(flared::daemon::run_loop(
                    Arc::clone(&state.cfg),
                    Arc::clone(&state.store),
                    Arc::clone(&state.log),
                    state.snapshot.clone(),
                ));
                let result = flared::http::serve(state).await;
                loop_handle.abort();
                result
            })?;
        }
        Command::Service { command: ServiceCommand::Print } => {
            let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("flared"));
            let platform = if cfg!(windows) {
                "windows"
            } else if cfg!(target_os = "macos") {
                "macos"
            } else if cfg!(target_os = "linux") {
                "linux"
            } else {
                // Preserve the manual-run fallback on other unixes.
                std::env::consts::OS
            };
            println!("{}", flared::service::autostart_recipe(platform, &exe));
        }
    }
    Ok(())
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1u64 << 30) as f64
}
