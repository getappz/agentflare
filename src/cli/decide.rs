use clap::{Args, Subcommand};
use serde_json::json;
use std::collections::BTreeMap;

use crate::decide::{self, Question};

/// Typed-decision layer (Jev): check that the connection works.
#[derive(Args)]
pub struct DecideArgs {
    #[command(subcommand)]
    cmd: DecideCmd,
}

#[derive(Subcommand)]
enum DecideCmd {
    /// Send one tiny test decision and print the answer, latency and cost.
    /// Needs AGENTFLARE_JEV=1 plus OPENROUTER_API_KEY (or Cloudflare creds),
    /// looked up in the environment, then the agentflare vault, then ~/.env.
    /// Store the key with: agentflare vault set OPENROUTER_API_KEY
    Ping,
    /// Show how often Jev agrees with the current decision, per site
    /// (router, skill_rerank, sdd_judge), from the shadow log.
    Report {
        /// Only this decision site.
        #[arg(long)]
        site: Option<String>,
    },
    /// The opt-in local training dataset (AGENTFLARE_DECIDE_CAPTURE=1). It
    /// contains truncated, redacted user prompt text; it never leaves this
    /// machine.
    Dataset {
        #[command(subcommand)]
        cmd: DatasetCmd,
    },
}

#[derive(Subcommand)]
enum DatasetCmd {
    /// Rows per site, label balance, date range, repetition rate and top-k
    /// key coverage (where a deterministic engine would pay off).
    Stats,
    /// Delete the captured dataset.
    Clear,
}

impl DecideArgs {
    pub fn run(self) {
        match self.cmd {
            DecideCmd::Ping => ping(),
            DecideCmd::Report { site } => {
                let rows = decide::shadow::load(&decide::shadow::log_path());
                print!(
                    "{}",
                    decide::shadow::render(&decide::shadow::summarize(&rows, site.as_deref()))
                );
            }
            DecideCmd::Dataset { cmd } => {
                let path = decide::capture::dataset_path();
                match cmd {
                    DatasetCmd::Stats => {
                        let rows = decide::capture::load(&path);
                        print!(
                            "{}",
                            decide::capture::render(&decide::capture::summarize(&rows))
                        );
                    }
                    DatasetCmd::Clear => match decide::capture::clear(&path) {
                        Ok(n) => println!("removed {n} dataset file(s)"),
                        Err(e) => {
                            eprintln!("failed to clear dataset: {e}");
                            std::process::exit(1);
                        }
                    },
                }
            }
        }
    }
}

fn ping() {
    for (name, source) in decide::credential_sources() {
        println!("{name}: from {}", source.label());
    }
    let state = json!("Help! My payouts have been failing for 3 days.");
    let questions = BTreeMap::from([(
        "is_urgent".to_string(),
        Question::noul("Does this message convey urgency?"),
    )]);
    match decide::ask(&state, &questions) {
        Ok(out) => {
            for (id, answer) in &out.response.answers {
                println!("{id}: {}", answer.summary());
            }
            let u = &out.response.usage;
            let cost = u.cost.map_or("n/a".to_string(), |c| format!("${c:.7}"));
            println!(
                "{} ms, {} in / {} out tokens, cost {cost}, model {}",
                out.elapsed_ms,
                u.input_tokens,
                u.output_tokens,
                out.response.model.as_deref().unwrap_or("?")
            );
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
