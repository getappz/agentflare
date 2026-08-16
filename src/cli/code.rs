use clap::{Args, Subcommand};
use std::io::Read;
use std::path::PathBuf;
use std::process::exit;

/// Static analysis for the current repo (currently: change-impact).
#[derive(Args)]
pub struct CodeArgs {
    #[command(subcommand)]
    pub command: CodeCommand,
}

#[derive(Subcommand)]
pub enum CodeCommand {
    /// Blast radius of a file: what depends on it, including same-crate
    /// integration tests and confirmed cross-crate references. Rust
    /// (Cargo workspace) projects only, for now.
    Impact(ImpactArgs),
}

#[derive(Args)]
pub struct ImpactArgs {
    /// File path. Omit when using --stdin.
    pub target: Option<PathBuf>,
    /// Read a newline-separated file list from stdin (e.g. pipe
    /// `git diff --name-only`) and report the union of their impact.
    #[arg(long)]
    pub stdin: bool,
    /// Emit JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: CodeArgs) {
    match args.command {
        CodeCommand::Impact(opts) => impact_cmd(opts),
    }
}

fn impact_cmd(opts: ImpactArgs) {
    let repo_root = std::env::current_dir().expect("cwd");

    let targets: Vec<PathBuf> = if opts.stdin {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .expect("reading stdin");
        buf.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect()
    } else {
        match opts.target {
            Some(t) => vec![t],
            None => {
                crate::ui::error("agentflare code impact: pass a file path, or --stdin");
                exit(2);
            }
        }
    };

    let mut reports = Vec::new();
    for target in &targets {
        match crate::code::impact_for_path(&repo_root, target) {
            Ok(report) => reports.push(report),
            Err(e) => {
                crate::ui::error(&format!("agentflare code impact: {e}"));
                exit(1);
            }
        }
    }

    if opts.json {
        print_json(&reports);
    } else {
        print_text(&reports);
    }
}

fn print_text(reports: &[crate::code::ImpactReport]) {
    for report in reports {
        println!(
            "{} (crate `{}`) — {} dependent(s)",
            report.target.display(),
            report.owner_crate,
            report.hits.len()
        );
        for hit in &report.hits {
            let reason = match &hit.reason {
                crate::code::ImpactReason::OwnIntegrationTest => "own test".to_string(),
                crate::code::ImpactReason::CrossCrateReference { via_crate } => {
                    format!("via `{via_crate}`")
                }
            };
            println!(
                "  {}:{} [{reason}] {}",
                hit.file.display(),
                hit.line,
                hit.text
            );
        }
    }
}

fn print_json(reports: &[crate::code::ImpactReport]) {
    let value: Vec<_> = reports
        .iter()
        .map(|report| {
            serde_json::json!({
                "target": report.target,
                "owner_crate": report.owner_crate,
                "hits": report.hits.iter().map(|h| {
                    let (reason, via_crate) = match &h.reason {
                        crate::code::ImpactReason::OwnIntegrationTest => ("own_test", None),
                        crate::code::ImpactReason::CrossCrateReference { via_crate } => {
                            ("cross_crate", Some(via_crate.clone()))
                        }
                    };
                    serde_json::json!({
                        "file": h.file,
                        "line": h.line,
                        "text": h.text,
                        "reason": reason,
                        "via_crate": via_crate,
                    })
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
}
