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
}

impl DecideArgs {
    pub fn run(self) {
        match self.cmd {
            DecideCmd::Ping => ping(),
        }
    }
}

fn ping() {
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
