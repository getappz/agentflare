//! DEPRECATED — use `agentflare flare output` instead.
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum CavemanAction {
    Compress {
        source: PathBuf,
        target: Option<PathBuf>,
        #[arg(long)]
        spec_file: Option<PathBuf>,
        #[arg(long)]
        backup: Option<String>,
    },
}

#[derive(Args)]
pub struct CavemanArgs {
    #[command(subcommand)]
    pub action: CavemanAction,
}

impl CavemanArgs {
    pub fn run(self) {
        match self.action {
            CavemanAction::Compress {
                source,
                target,
                spec_file,
                backup,
            } => {
                let target = target.unwrap_or_else(|| source.clone());
                let prompt = match &spec_file {
                    Some(path) => match std::fs::read_to_string(path) {
                        Ok(spec) => crate::optimize::Prompt::Custom(spec),
                        Err(e) => {
                            eprintln!("failed to read spec file {}: {e}", path.display());
                            std::process::exit(1);
                        }
                    },
                    None => crate::optimize::Prompt::Generic,
                };
                let backup_mode = match backup.as_deref() {
                    Some("sibling") => crate::optimize::BackupMode::Sibling,
                    Some("out-of-tree") | None => crate::optimize::BackupMode::OutOfTree,
                    Some(other) => {
                        eprintln!("--backup must be 'sibling' or 'out-of-tree', got '{other}'");
                        std::process::exit(1);
                    }
                };
                let result = crate::optimize::compress(
                    &crate::optimize::RealLlm,
                    &source,
                    &target,
                    prompt,
                    backup_mode,
                );
                match result {
                    Ok(report) => {
                        let pct =
                            100 - (100 * report.compressed_bytes / report.original_bytes.max(1));
                        println!(
                            "{}→{}B ▼{pct}%",
                            report.original_bytes, report.compressed_bytes
                        );
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}
