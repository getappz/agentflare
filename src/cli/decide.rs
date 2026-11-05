use clap::{Args, Subcommand};
use serde_json::json;
use std::collections::BTreeMap;

use crate::decide::bench;
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
    /// Benchmark the offline flare-decide engines on the built-in cases.
    /// The "Jev" row is simulated, and the cases overlap the rules, so
    /// treat the numbers as a smoke test, not a quality comparison.
    Bench {
        /// Replay a decision site (router, skill_rerank, sdd_judge, or all)
        /// through Jev and its non-Jev baseline; omit for the offline engines.
        site: Option<String>,
        /// Labeled/adversarial JSONL fixtures (default: the built-in set).
        #[arg(long)]
        fixtures: Vec<std::path::PathBuf>,
        /// Replay the captured dataset (AGENTFLARE_DECIDE_CAPTURE=1) instead.
        #[arg(long)]
        captured: bool,
        /// Go/no-go thresholds TOML (see docs/decide-bench-thresholds.toml).
        #[arg(long)]
        thresholds: Option<std::path::PathBuf>,
        /// Exit 1 when any row is NO-GO.
        #[arg(long)]
        enforce: bool,
        #[arg(long)]
        json: bool,
        /// Fake backend: virtual cold / warm latency, jitter (ms) and seed.
        #[arg(long, default_value_t = 1200)]
        fake_cold_ms: u64,
        #[arg(long, default_value_t = 350)]
        fake_warm_ms: u64,
        #[arg(long, default_value_t = 100)]
        fake_jitter_ms: u64,
        #[arg(long, default_value_t = 7)]
        seed: u64,
    },
    /// Show how often Jev agrees with the current decision, per site
    /// (router, skill_rerank, sdd_judge), from the shadow log.
    Report {
        /// Only this decision site.
        #[arg(long)]
        site: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Append a later ground-truth answer to the retained decision ledger.
    Outcome {
        #[command(subcommand)]
        cmd: OutcomeCmd,
    },
    /// Delete decisions, outcomes, dropped-write counts and captured inputs.
    Clear,
    /// The opt-in local training dataset (AGENTFLARE_DECIDE_CAPTURE=1). It
    /// contains truncated, redacted user prompt text; it never leaves this
    /// machine.
    Dataset {
        #[command(subcommand)]
        cmd: DatasetCmd,
    },
}

#[derive(Subcommand)]
enum OutcomeCmd {
    Add {
        #[arg(long)]
        decision_id: String,
        /// human_override, pr_review_result, item_final_state, incident_confirmed
        #[arg(long)]
        kind: String,
        /// Correct answer in the same domain as the report's baseline/Jev answer.
        #[arg(long)]
        answer: String,
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
            DecideCmd::Bench { site: None, .. } => print!(
                "{}",
                flare_decide::bench::format_markdown_table(
                    &flare_decide::bench::run_all_benchmarks()
                )
            ),
            DecideCmd::Bench {
                site: Some(site),
                fixtures,
                captured,
                thresholds,
                enforce,
                json,
                fake_cold_ms,
                fake_warm_ms,
                fake_jitter_ms,
                seed,
            } => {
                let profile = bench::Profile {
                    cold_ms: fake_cold_ms,
                    warm_ms: fake_warm_ms,
                    jitter_ms: fake_jitter_ms,
                    seed,
                    ..bench::Profile::default()
                };
                match bench_site(
                    &site,
                    &fixtures,
                    captured,
                    thresholds.as_deref(),
                    profile,
                    json,
                ) {
                    Ok(go) if go || !enforce => {}
                    Ok(_) => std::process::exit(1),
                    Err(e) => {
                        eprintln!("decide bench: {e}");
                        std::process::exit(2);
                    }
                }
            }
            DecideCmd::Report { site, json } => {
                let report = decide::report::load(
                    &decide::shadow::log_path(),
                    &decide::capture::dataset_path(),
                    site.as_deref(),
                );
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&report).expect("finite report")
                    );
                } else {
                    println!("breaker: {}", decide::breaker::status());
                    print!("{}", decide::report::render(&report));
                }
            }
            DecideCmd::Outcome {
                cmd:
                    OutcomeCmd::Add {
                        decision_id,
                        kind,
                        answer,
                    },
            } => {
                let result = serde_json::from_value(serde_json::Value::String(kind))
                    .map_err(|e| e.to_string())
                    .and_then(|kind| {
                        decide::tracking::outcome_add(
                            &decide::shadow::log_path(),
                            &decision_id,
                            kind,
                            &answer,
                        )
                    });
                if let Err(e) = result {
                    eprintln!("cannot label decision: {e}");
                    std::process::exit(1);
                }
                println!("outcome added for {decision_id}");
            }
            DecideCmd::Clear => {
                for path in [decide::shadow::log_path(), decide::capture::dataset_path()] {
                    if let Err(e) = decide::capture::clear(&path) {
                        eprintln!("failed to clear decisions: {e}");
                        std::process::exit(1);
                    }
                }
                println!("decision ledger and dataset cleared");
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
    println!("breaker: {}", decide::breaker::status());
    for (name, source) in decide::credential_sources() {
        println!("{name}: from {}", source.label());
    }
    let state = json!("Help! My payouts have been failing for 3 days.");
    let questions = BTreeMap::from([(
        "is_urgent".to_string(),
        Question::noul("Does this message convey urgency?"),
    )]);
    match decide::tracking::ask_with(
        "ping",
        "none",
        |_| false,
        &state,
        &questions,
        || decide::ask(&state, &questions),
    )
    .result
    {
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
            decide::exit(1);
        }
    }
}

/// Returns whether every row is GO.
fn bench_site(
    site: &str,
    fixtures: &[std::path::PathBuf],
    captured: bool,
    thresholds: Option<&std::path::Path>,
    profile: bench::Profile,
    json: bool,
) -> Result<bool, String> {
    let sites: Vec<&str> = if site == "all" {
        bench::SITES.to_vec()
    } else if bench::SITES.contains(&site) {
        vec![site]
    } else {
        return Err(format!(
            "unknown site '{site}' (router, skill_rerank, sdd_judge, all)"
        ));
    };
    let thresholds = match thresholds {
        Some(p) => bench::Thresholds::parse(
            &std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?,
        )
        .map_err(|e| format!("{}: {e}", p.display()))?,
        None => bench::Thresholds::default(),
    };
    let mut cases = vec![];
    if captured {
        cases.extend(
            decide::capture::load(&decide::capture::dataset_path())
                .into_iter()
                .filter_map(|r| serde_json::from_value(serde_json::to_value(r).ok()?).ok()),
        );
    }
    for p in fixtures {
        let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
        cases.extend(bench::parse_cases(&text).0);
    }
    if !captured && fixtures.is_empty() {
        for s in &sites {
            cases.extend(bench::builtin_cases(s));
        }
    }
    let mut backend = bench::FakeBackend::new(profile);
    let report = bench::Report {
        schema: bench::REPORT_SCHEMA,
        backend: bench::Backend::name(&backend),
        sites: sites
            .iter()
            .flat_map(|s| bench::run_site(s, &cases, &mut backend, &thresholds))
            .collect(),
    };
    if json {
        println!("{}", bench::render_json(&report));
    } else {
        print!("{}", bench::render_table(&report));
    }
    Ok(report.sites.iter().all(|s| s.verdict.go))
}
