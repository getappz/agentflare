//! `insights reflect`: turn mined candidates into a decision ledger (scan/accept/reject/snooze).

use std::collections::{BTreeMap, BTreeSet};
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
        #[arg(long, default_value = "30", value_parser = clap::value_parser!(u32).range(1..=36_500))]
        days: u32,
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
            r"^don['’]?t ",
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

/// One distinct correction, aggregated across the turns that said it.
struct CorrectionGroup {
    title: String,
    text: String,
    sessions: BTreeSet<String>,
    occurrences: i64,
    evidence: Vec<EvidenceRef>,
}

/// Correction pass over user turns: each distinct normalised correction is one candidate,
/// counted across every turn and session before the upsert, so rescans stay idempotent.
fn scan_corrections(store: &InsightsStore, days: u32) -> Result<usize, Box<dyn std::error::Error>> {
    let since = Utc::now().checked_sub_signed(Duration::days(days as i64));
    let mut groups: BTreeMap<(String, String), CorrectionGroup> = BTreeMap::new();
    for s in store.sessions_in_window(since, None, None, None)? {
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
            let ev = EvidenceRef {
                source: s.source.as_str().into(),
                session_id: s.id.clone(),
                turn_seq: Some(t.seq),
                tool_call_id: None,
                at: None,
                excerpt: None,
            }
            .with_excerpt(&text);
            let group = groups
                .entry((s.project.clone(), text.clone()))
                .or_insert_with(|| CorrectionGroup {
                    title: text.chars().take(80).collect(),
                    text: text.clone(),
                    sessions: BTreeSet::new(),
                    occurrences: 0,
                    evidence: Vec::new(),
                });
            group.sessions.insert(s.id.clone());
            group.occurrences += 1;
            group.evidence.push(ev);
        }
    }
    let n = groups.len();
    for ((project, key), g) in groups {
        store.upsert_proposal_candidate(&candidate(
            "memory_fact",
            &project,
            &key,
            &g.title,
            &g.text,
            g.sessions.len() as i64,
            g.occurrences,
            g.evidence,
        ))?;
    }
    Ok(n)
}

/// Upsert a mined cluster. Its identity must survive new evidence and a changed destination:
/// `cluster_id` and the draft both move when a member joins, so identity comes from member
/// text alone. Every member seen is recorded as an alias of the proposal, so a rejected
/// proposal stays rejected even after the members it was first seen with leave the cluster.
fn upsert_mined(
    store: &InsightsStore,
    p: &flare_insights::discover::Proposal,
) -> Result<(), Box<dyn std::error::Error>> {
    const MINED: &str = "mined";
    let mut aliases: Vec<String> = p
        .examples
        .iter()
        .filter_map(|e| e.excerpt.as_deref())
        .map(|text| fingerprint(MINED, "", text))
        .collect();
    aliases.sort_unstable();
    aliases.dedup();
    let kind = format!("{:?}", p.destination).to_lowercase();
    let mut c = candidate(
        &kind,
        "",
        &p.draft,
        &p.draft.chars().take(80).collect::<String>(),
        &p.draft,
        p.distinct_sessions as i64,
        p.occurrences as i64,
        p.examples.clone(),
    );
    c.fingerprint = match store.proposal_for_aliases(&aliases)? {
        Some(existing) => existing,
        None => fingerprint(MINED, "", &p.draft),
    };
    store.upsert_proposal_candidate(&c)?;
    store.add_proposal_aliases(&c.fingerprint, &aliases)?;
    Ok(())
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
        // The fingerprint keeps two corrections with the same slug from sharing one memory.
        let topic = format!("insights/{}/{}-{}", p.kind, slug(&p.title), p.fingerprint);
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
                    upsert_mined(&store, p).unwrap_or_else(|e| fail(e));
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
                Some(Utc::now() + Duration::days(i64::from(days))),
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
    use flare_insights::discover::{Destination, Proposal};

    #[test]
    fn corrections_match_expected_phrases() {
        assert!(is_correction("no, use pnpm here"));
        assert!(is_correction("don't mock the db"));
        assert!(is_correction("don’t mock the db"));
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
        let fp = store.get_proposal(id).unwrap().unwrap().fingerprint;
        assert_eq!(topic, format!("insights/memory_fact/use-pnpm-{fp}"));
        assert_eq!(
            store.get_proposal(id).unwrap().unwrap().status,
            ProposalStatus::Accepted
        );
    }

    fn mined(draft: &str, members: &[&str], destination: Destination) -> Proposal {
        Proposal {
            cluster_id: format!("cluster-{}", members.len()),
            destination,
            distinct_sessions: members.len(),
            eligible_sessions: members.len(),
            occurrences: members.len(),
            examples: members
                .iter()
                .enumerate()
                .map(|(i, text)| {
                    EvidenceRef {
                        source: "codex".into(),
                        session_id: format!("s{i}"),
                        turn_seq: Some(0),
                        tool_call_id: None,
                        at: None,
                        excerpt: None,
                    }
                    .with_excerpt(text)
                })
                .collect(),
            draft: draft.into(),
            rationale: String::new(),
        }
    }

    #[test]
    fn rejected_mined_proposal_stays_rejected_when_the_cluster_changes() {
        let store = InsightsStore::open_in_memory().unwrap();
        let first = mined("run the suite", &["run the suite"], Destination::Skill);
        upsert_mined(&store, &first).unwrap();
        let id = store.list_proposals(None).unwrap()[0].id;
        store
            .set_proposal_status(id, ProposalStatus::Rejected, None)
            .unwrap();
        // A new member sorts first, so the draft, cluster id and destination all change.
        let grown = mined(
            "again run the suite",
            &["again run the suite", "run the suite"],
            Destination::Rule,
        );
        upsert_mined(&store, &grown).unwrap();
        let rows = store.list_proposals(None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, ProposalStatus::Rejected);
        assert_eq!(rows[0].occurrence_count, 2);
    }

    #[test]
    fn rejection_survives_a_chain_of_overlapping_rescans() {
        let store = InsightsStore::open_in_memory().unwrap();
        upsert_mined(&store, &mined("aaa", &["aaa"], Destination::Skill)).unwrap();
        let id = store.list_proposals(None).unwrap()[0].id;
        store
            .set_proposal_status(id, ProposalStatus::Rejected, None)
            .unwrap();
        upsert_mined(&store, &mined("aaa", &["aaa", "bbb"], Destination::Skill)).unwrap();
        upsert_mined(&store, &mined("bbb", &["bbb", "ccc"], Destination::Skill)).unwrap();
        let rows = store.list_proposals(None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, ProposalStatus::Rejected);
    }

    #[test]
    fn open_mined_proposal_follows_a_changed_destination() {
        let store = InsightsStore::open_in_memory().unwrap();
        upsert_mined(&store, &mined("aaa", &["aaa"], Destination::Skill)).unwrap();
        upsert_mined(&store, &mined("aaa", &["aaa", "bbb"], Destination::Rule)).unwrap();
        assert_eq!(store.list_proposals(None).unwrap()[0].kind, "rule");
    }

    #[test]
    fn long_corrections_differing_late_stay_separate() {
        use flare_insights::model::*;
        let store = InsightsStore::open_in_memory().unwrap();
        store
            .upsert_session(&Session {
                id: "a".into(),
                source: SessionSource::Codex,
                project: "demo".into(),
                project_path: None,
                title: None,
                model: None,
                status: SessionStatus::Completed,
                awaiting_reason: None,
                started_at: None,
                updated_at: Utc::now(),
                ended_at: None,
                duration_secs: None,
                tokens: TokenUsage {
                    input: 0,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                },
                cost: None,
                cost_basis: CostBasis::Unknown,
                pricing_key: None,
                turn_count: 0,
                tool_call_count: 0,
                subagent_count: 0,
                tags: vec![],
                starred: false,
                pid: None,
                cwd: None,
            })
            .unwrap();
        let shared = format!("don't {}", "x".repeat(250));
        let turns: Vec<Turn> = ["one", "two"]
            .iter()
            .enumerate()
            .map(|(i, tail)| Turn {
                id: format!("t{i}"),
                session_id: "a".into(),
                seq: i as u32,
                user_text: Some(format!("{shared} {tail}")),
                assistant_text: None,
                started_at: None,
                ended_at: None,
                tokens: None,
                cost_usd: None,
            })
            .collect();
        store.upsert_turns_batch(&turns).unwrap();
        assert_eq!(scan_corrections(&store, 30).unwrap(), 2);
        assert_eq!(store.list_proposals(None).unwrap().len(), 2);
    }

    #[test]
    fn corrections_aggregate_turns_and_sessions_and_rescan_is_idempotent() {
        use flare_insights::model::*;
        let store = InsightsStore::open_in_memory().unwrap();
        let session = |id: &str| Session {
            id: id.into(),
            source: SessionSource::Codex,
            project: "demo".into(),
            project_path: None,
            title: None,
            model: None,
            status: SessionStatus::Completed,
            awaiting_reason: None,
            started_at: None,
            updated_at: Utc::now(),
            ended_at: None,
            duration_secs: None,
            tokens: TokenUsage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost: None,
            cost_basis: CostBasis::Unknown,
            pricing_key: None,
            turn_count: 0,
            tool_call_count: 0,
            subagent_count: 0,
            tags: vec![],
            starred: false,
            pid: None,
            cwd: None,
        };
        let turn = |sid: &str, seq: u32| Turn {
            id: format!("{sid}-{seq}"),
            session_id: sid.into(),
            seq,
            user_text: Some("don't mock the database please".into()),
            assistant_text: None,
            started_at: None,
            ended_at: None,
            tokens: None,
            cost_usd: None,
        };
        for sid in ["a", "b"] {
            store.upsert_session(&session(sid)).unwrap();
        }
        store
            .upsert_turns_batch(&[turn("a", 0), turn("a", 1), turn("b", 0)])
            .unwrap();
        for _ in 0..2 {
            assert_eq!(scan_corrections(&store, 30).unwrap(), 1);
            let rows = store.list_proposals(None).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!((rows[0].session_count, rows[0].occurrence_count), (2, 3));
        }
    }

    #[test]
    fn snooze_days_must_be_in_range() {
        for bad in ["0", "-1", "99999999"] {
            assert!(
                ReflectArgs::try_parse_from(["reflect", "snooze", "1", "--days", bad]).is_err()
            );
        }
        assert!(ReflectArgs::try_parse_from(["reflect", "snooze", "1", "--days", "7"]).is_ok());
    }
}
