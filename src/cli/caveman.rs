use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum CavemanAction {
    /// Compress a markdown file. With no --spec-file, uses caveman's own
    /// generic compression prompt and backs up out-of-tree. With
    /// --spec-file, uses the given spec text as the compression prompt
    /// (used by short-skill) and defaults to a sibling backup.
    Compress {
        source: PathBuf,
        /// Defaults to `source` (in-place) when omitted.
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
            CavemanAction::Compress { source, target, spec_file, backup } => {
                let target = target.unwrap_or_else(|| source.clone());
                let prompt = match &spec_file {
                    Some(path) => match std::fs::read_to_string(path) {
                        Ok(spec) => caveman::Prompt::Custom(spec),
                        Err(e) => {
                            eprintln!("failed to read spec file {}: {e}", path.display());
                            std::process::exit(1);
                        }
                    },
                    None => caveman::Prompt::Generic,
                };
                let backup_mode = match backup.as_deref() {
                    Some("sibling") => caveman::BackupMode::Sibling,
                    Some("out-of-tree") | None => caveman::BackupMode::OutOfTree,
                    Some(other) => {
                        eprintln!("--backup must be 'sibling' or 'out-of-tree', got '{other}'");
                        std::process::exit(1);
                    }
                };
                let result = caveman::compress(&caveman::RealLlm, &source, &target, prompt, backup_mode);
                match result {
                    Ok(report) => {
                        let ratio = 100 * report.compressed_bytes / report.original_bytes.max(1);
                        let pct = 100usize.saturating_sub(ratio);
                        println!("{}→{}B ▼{pct}%", report.original_bytes, report.compressed_bytes);
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
