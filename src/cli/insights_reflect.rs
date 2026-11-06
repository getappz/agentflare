//! `insights reflect`: turn mined candidates into a decision ledger (scan/accept/reject/snooze).

use std::path::PathBuf;
use std::sync::OnceLock;

use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};
use flare_insights::evidence::EvidenceRef;
use flare_insights::proposals::{ProposalRow, ProposalStatus, fingerprint};
use flare_insights::store::InsightsStore;
use regex::Regex;

#[derive(Parser, Debug)]
pub struct ReflectArgs {
    #[command(subcommand)]
    pub command: ReflectCommands,
}

#[derive(Subcommand, Debug)]
pub enum ReflectCommands {
    /// Mine recurring requests and corrections into the proposals ledger
    Scan {
        #[arg(long, default_value = "30")]
        days: u32,
    },
    /// Accept a proposal (memory_fact proposals are saved to memory)
    Accept { id: i64 },
    /// Reject a proposal; rescans will not resurface it
    Reject { id: i64 },
    /// Hide a proposal for a while
    Snooze {
        id: i64,
        #[arg(long, default_value = "30")]
        days: i64,
    },
    /// List proposals
    List {
        #[arg(long)]
        status: Option<String>,
    },
    /// Show one proposal
    Show { id: i64 },
}

fn correction_res() -> &'static [Regex] {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    RES.get_or_init(|| {
        [
            r"^no[, ]+use ",
            r"^don'?t ",
            r"^stop ",
            r"^always ",
            r"^never ",
            r"instead of ",
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    })
}

fn is_correction(text: &str) -> bool {
    correction_res().iter().any(|r| r.is_match(text))
}

/// Correction pass over user turns: each distinct normalised correction is one candidate.
fn scan_corrections(store: &InsightsStore, days: u32) -> Result<usize, Box<dyn std::error::Error>> {
    let since = Utc::now() - Duration::days(days as i64);
    let mut n = 0;
    for s in store.sessions_in_window(Some(since), None, None, None)? {
        for t in store.get_turns(&s.id)? {
            let Some(raw) = t.user_text.as_deref() else {
                continue;
            };
            let Some(text) = flare_insights::forensics::normalise_user_text(raw) else {
                continue;
            };
            let text = text.trim().to_lowercase();
            if !is_correction(&text) {
                continue;
            }
            let title: String = text.chars().take(80).collect();
            let key: String = text.chars().take(200).collect();
            let ev = EvidenceRef {
                source: s.source.as_str().into(),
                session_id: s.id.clone(),
                turn_seq: Some(t.seq),
                tool_call_id: None,
                at: None,
                excerpt: None,
            }
            .with_excerpt(&text);
            store.upsert_proposal_candidate(&candidate(
                "memory_fact",
                &s.project,
                &key,
                &title,
                &text,
                1,
                1,
                vec![ev],
            ))?;
            n += 1;
        }
    }
    Ok(n)
}

#[allow(clippy::too_many_arguments)]
fn candidate(
    kind: &str,
    project: &str,
    key: &str,
    title: &str,
    content: &str,
    sessions: i64,
    occurrences: i64,
    evidence: Vec<EvidenceRef>,
) -> ProposalRow {
    let now = Utc::now();
    ProposalRow {
        id: 0,
        fingerprint: fingerprint(kind, project, key),
        kind: kind.into(),
        project: project.into(),
        title: title.into(),
        proposed_content: content.into(),
        target_path: None,
        status: ProposalStatus::Proposed,
        session_count: sessions,
        occurrence_count: occurrences,
        evidence,
        model_context: None,
        applied_hash: None,
        created_at: now,
        updated_at: now,
        snoozed_until: None,
    }
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').chars().take(60).collect()
}

/// Accept a proposal; `memory_fact` ones are promoted into brain.db with a topic key.
pub(crate) fn accept(
    store: &InsightsStore,
    conn: &rusqlite::Connection,
    id: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let p = store
        .get_proposal(id)?
        .ok_or_else(|| format!("no proposal {id}"))?;
    if p.kind == "memory_fact" {
        let topic = format!("insights/{}/{}", p.kind, slug(&p.title));
        let project = (!p.project.is_empty()).then_some(p.project.as_str());
        crate::memory::observations::save_keyed(
            conn,
            None,
            "learning",
            &p.title,
            &p.proposed_content,
            None,
            project,
            Some("project"),
            Some(&topic),
            Some(&p.fingerprint),
        )?;
    }
    store.set_proposal_status(id, ProposalStatus::Accepted, None)?;
    Ok(())
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}

impl ReflectArgs {
    pub fn run(self, db: PathBuf) {
        let store = InsightsStore::open(&db).unwrap_or_else(|e| fail(e));
        let decide =
            |id: i64, st: ProposalStatus, until| match store.set_proposal_status(id, st, until) {
                Ok(true) => println!("proposal {id}: {}", st.as_str()),
                Ok(false) => fail(format!("no proposal {id}")),
                Err(e) => fail(e),
            };
        match self.command {
            ReflectCommands::Scan { days } => {
                let opts = flare_insights::discover::DiscoverOptions {
                    days,
                    ..Default::default()
                };
                let report = flare_insights::discover::mine(&store, &opts, Utc::now())
                    .unwrap_or_else(|e| fail(format!("discover failed: {e}")));
                for p in &report.body {
                    let kind = format!("{:?}", p.destination).to_lowercase();
                    let c = candidate(
                        &kind,
                        "",
                        &p.cluster_id,
                        &p.draft.chars().take(80).collect::<String>(),
                        &p.draft,
                        p.distinct_sessions as i64,
                        p.occurrences as i64,
                        p.examples.clone(),
                    );
                    store
                        .upsert_proposal_candidate(&c)
                        .unwrap_or_else(|e| fail(e));
                }
                let corrections = scan_corrections(&store, days).unwrap_or_else(|e| fail(e));
                println!(
                    "scan: {} recurring, {corrections} corrections; {} proposals in ledger",
                    report.body.len(),
                    store.list_proposals(None).map(|v| v.len()).unwrap_or(0)
                );
            }
            ReflectCommands::Accept { id } => {
                let conn = crate::memory::store::open().unwrap_or_else(|e| fail(e));
                accept(&store, &conn, id).unwrap_or_else(|e| fail(e));
                println!("proposal {id}: accepted");
            }
            ReflectCommands::Reject { id } => decide(id, ProposalStatus::Rejected, None),
            ReflectCommands::Snooze { id, days } => decide(
                id,
                ProposalStatus::Snoozed,
                Some(Utc::now() + Duration::days(days)),
            ),
            ReflectCommands::List { status } => {
                let st = status.as_deref().map(|s| {
                    ProposalStatus::parse(s).unwrap_or_else(|| fail(format!("bad status {s}")))
                });
                for p in store.list_proposals(st).unwrap_or_else(|e| fail(e)) {
                    println!(
                        "{:>4} {:<10} {:<11} s={} n={} {}",
                        p.id,
                        p.status.as_str(),
                        p.kind,
                        p.session_count,
                        p.occurrence_count,
                        p.title
                    );
                }
            }
            ReflectCommands::Show { id } => match store.get_proposal(id) {
                Ok(Some(p)) => println!("{}", serde_json::to_string_pretty(&p).unwrap()),
                Ok(None) => fail(format!("no proposal {id}")),
                Err(e) => fail(e),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrections_match_expected_phrases() {
        assert!(is_correction("no, use pnpm here"));
        assert!(is_correction("don't mock the db"));
        assert!(is_correction("use rg instead of grep"));
        assert!(!is_correction("please add a test"));
    }

    #[test]
    fn accept_memory_fact_writes_observation_with_topic_key() {
        let store = InsightsStore::open_in_memory().unwrap();
        let c = candidate(
            "memory_fact",
            "demo",
            "k",
            "Use pnpm",
            "no, use pnpm",
            1,
            1,
            vec![],
        );
        store.upsert_proposal_candidate(&c).unwrap();
        let id = store.list_proposals(None).unwrap()[0].id;
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::memory::schema::migrate(&mut conn).unwrap();
        accept(&store, &conn, id).unwrap();
        let topic: String = conn
            .query_row(
                "SELECT topic_key FROM observations WHERE title='Use pnpm'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(topic, "insights/memory_fact/use-pnpm");
        assert_eq!(
            store.get_proposal(id).unwrap().unwrap().status,
            ProposalStatus::Accepted
        );
    }
}
