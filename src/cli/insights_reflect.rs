//! `insights reflect`: turn mined candidates into a decision ledger (scan/accept/reject/snooze/apply).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};
use flare_insights::discover::member_key;
use flare_insights::evidence::EvidenceRef;
use flare_insights::proposals::{ProposalRow, ProposalStatus, fingerprint};
use flare_insights::store::InsightsStore;
use regex::Regex;
use sha2::{Digest, Sha256};

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
    /// Preview (default) or write a proposal into its config/skill target
    Apply {
        id: i64,
        /// Actually write; without this flag only a unified diff is printed
        #[arg(long)]
        yes: bool,
        /// Allow targets under $HOME/.claude
        #[arg(long)]
        global: bool,
    },
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
    let aliases: Vec<String> = p
        .member_keys
        .iter()
        .map(|key| fingerprint(MINED, "", key))
        .collect();
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
        None => fingerprint(MINED, "", &member_key(&p.draft)),
    };
    store.upsert_proposal_candidate(&c)?;
    store.add_proposal_aliases(&c.fingerprint, &aliases)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn candidate(
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
        let evidence = super::memory_consolidate::evidence_ids(&p.proposed_content);
        // The fingerprint keeps two corrections with the same slug from sharing one memory.
        // A consolidated belief's title follows its representative member, so its
        // identity is the fingerprint alone or a refinement would fork a second row.
        let topic = if evidence.is_empty() {
            format!("insights/{}/{}-{}", p.kind, slug(&p.title), p.fingerprint)
        } else {
            format!("insights/{}/belief-{}", p.kind, p.fingerprint)
        };
        let project = (!p.project.is_empty()).then_some(p.project.as_str());
        // A consolidated belief is refined on re-accept (topic_key upsert bumps
        // revision_count); the fingerprint idempotency key would no-op it.
        let idem = evidence.is_empty().then_some(p.fingerprint.as_str());
        let saved = crate::memory::observations::save_keyed(
            conn,
            None,
            "learning",
            &p.title,
            &p.proposed_content,
            None,
            project,
            Some("project"),
            Some(&topic),
            idem,
        )?;
        use crate::memory::observations::SaveOutcome::{Created, Duplicate, Updated};
        let (Created(belief) | Updated(belief) | Duplicate(belief)) = saved;
        super::memory_consolidate::link_evidence(conn, belief, &evidence)?;
    }
    store.set_proposal_status(id, ProposalStatus::Accepted, None)?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn read_target(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

/// Minimal unified diff (workspace `similar` is not a binary dep; keep this local).
fn unified_diff(path: &Path, before: &str, after: &str) -> String {
    let name = path.display().to_string();
    if before == after {
        return format!("--- {name}\n+++ {name}\n");
    }
    let old: Vec<&str> = before.split_inclusive('\n').collect();
    let new: Vec<&str> = after.split_inclusive('\n').collect();
    let mut out = format!(
        "--- {name}\n+++ {name}\n@@ -1,{} +1,{} @@\n",
        old.len(),
        new.len()
    );
    for line in &old {
        out.push('-');
        out.push_str(line);
        if !line.ends_with('\n') {
            out.push('\n');
        }
    }
    for line in &new {
        out.push('+');
        out.push_str(line);
        if !line.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

fn is_source_log_path(path: &Path) -> bool {
    // Normalised so the guard also holds for Windows separators.
    let s = path.to_string_lossy().replace('\\', "/");
    s.contains("/.claude/projects/")
        || s.contains("/.codex/")
        || s.contains("/.opencode/")
        || s.ends_with("state.vscdb")
        || s.ends_with("opencode.db")
        || s.ends_with(".jsonl")
}

fn under_global_claude(path: &Path) -> bool {
    dirs::home_dir().is_some_and(|home| path.starts_with(home.join(".claude")))
}

fn merge_learned(existing: &str, proposed: &str) -> String {
    let block = proposed.trim();
    if block.is_empty() {
        return existing.to_string();
    }
    let (mut pos, mut start, mut end) = (0, None, existing.len());
    for line in existing.split_inclusive('\n') {
        if start.is_none() {
            if line.trim_end() == "## Learned" {
                start = Some(pos);
            }
        } else if line.starts_with("# ") || line.starts_with("## ") {
            end = pos;
            break;
        }
        pos += line.len();
    }
    if let Some(start) = start {
        // Insert at the end of the section, which may not be the end of the file.
        let section = &existing[start..end];
        if section.contains(block) {
            return existing.to_string();
        }
        let (head, tail) = (&existing[..start], &existing[end..]);
        let gap = if tail.is_empty() { "" } else { "\n" };
        return format!("{head}{}\n\n{block}\n{gap}{tail}", section.trim_end());
    }
    let mut out = existing.trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str("## Learned\n\n");
    out.push_str(block);
    out.push('\n');
    out
}

fn skill_body(existing: &str, proposed: &str) -> Result<String, Box<dyn std::error::Error>> {
    let block = proposed.trim();
    if !existing.is_empty() && existing.contains(block) {
        return Ok(existing.to_string());
    }
    if !existing.is_empty() {
        return Err(
            "skill file already exists with different content; refusing to overwrite".into(),
        );
    }
    Ok(format!("{block}\n"))
}

fn build_new_content(
    path: &Path,
    kind: &str,
    existing: &str,
    proposed: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    if kind == "skill" || path.ends_with("SKILL.md") {
        skill_body(existing, proposed)
    } else {
        Ok(merge_learned(existing, proposed))
    }
}

/// Resolve exactly one write target for this proposal.
fn resolve_target(
    p: &ProposalRow,
    global: bool,
    cwd: &Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if p.target_path.is_none() {
        match p.kind.as_str() {
            "rule" | "skill" => {}
            "memory_fact" => {
                return Err(
                    "memory_fact proposals are promoted by `reflect accept`, not applied to a file"
                        .into(),
                );
            }
            k => return Err(format!("kind '{k}' has no applyable target").into()),
        }
    }
    let path = if let Some(tp) = p.target_path.as_deref() {
        let raw = PathBuf::from(tp);
        if raw.is_absolute() {
            raw
        } else {
            cwd.join(raw)
        }
    } else if p.kind == "skill" {
        let base = if global {
            dirs::home_dir()
                .ok_or("cannot resolve $HOME for --global")?
                .join(".claude")
        } else {
            cwd.join(".claude")
        };
        // A title with no ASCII alphanumerics slugs to nothing, which would drop the directory.
        let name = match slug(&p.title) {
            s if s.is_empty() => format!("skill-{}", &p.fingerprint[..12]),
            s => s,
        };
        base.join("skills").join(name).join("SKILL.md")
    } else {
        // Prefer an existing project doc; otherwise create .claude/rules/insights.md.
        let candidates = [
            cwd.join("AGENTS.md"),
            cwd.join("CLAUDE.md"),
            cwd.join(".claude/rules/insights.md"),
        ];
        candidates
            .into_iter()
            .find(|c| c.exists())
            .unwrap_or_else(|| cwd.join(".claude/rules/insights.md"))
    };

    if is_source_log_path(&path) {
        return Err("source logs are never an apply target".into());
    }
    if under_global_claude(&path) && !global {
        return Err(format!(
            "target {} is under $HOME/.claude; pass --global to allow",
            path.display()
        )
        .into());
    }
    Ok(path)
}

/// Re-hash the on-disk target and refuse if it drifted since preview; then atomic-write.
fn recheck_and_write(
    path: &Path,
    preview_hash: &str,
    new_content: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let now = read_target(path)?;
    let now_hash = sha256_hex(now.as_bytes());
    if now_hash != preview_hash {
        return Err(
            "target changed since preview; re-run apply without --yes to refresh the diff".into(),
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::atomic_fs::try_atomic_write(path, new_content.as_bytes(), None)?;
    Ok(sha256_hex(new_content.as_bytes()))
}

/// Dry-run (default) prints a unified diff; `--yes` hash-guards then writes one target.
pub(crate) fn apply(
    store: &InsightsStore,
    id: i64,
    yes: bool,
    global: bool,
    cwd: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let p = store
        .get_proposal(id)?
        .ok_or_else(|| format!("no proposal {id}"))?;
    if matches!(
        p.status,
        ProposalStatus::Rejected | ProposalStatus::Snoozed | ProposalStatus::Superseded
    ) {
        return Err(format!("proposal {id} is {}; not applying it", p.status.as_str()).into());
    }
    let path = resolve_target(&p, global, cwd)?;
    let before = read_target(&path)?;
    let preview_hash = sha256_hex(before.as_bytes());
    let after = build_new_content(&path, &p.kind, &before, &p.proposed_content)?;
    let diff = unified_diff(&path, &before, &after);
    if !yes {
        return Ok(diff);
    }
    let applied_hash = recheck_and_write(&path, &preview_hash, &after)?;
    if !store.set_proposal_applied(id, &applied_hash)? {
        return Err(format!("no proposal {id}").into());
    }
    Ok(diff)
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
            ReflectCommands::Apply { id, yes, global } => {
                let cwd = std::env::current_dir().unwrap_or_else(|e| fail(e));
                match apply(&store, id, yes, global, &cwd) {
                    Ok(diff) => {
                        print!("{diff}");
                        if yes {
                            println!("proposal {id}: applied");
                        }
                    }
                    Err(e) => fail(e),
                }
            }
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
            member_keys: members.iter().map(|text| member_key(text)).collect(),
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
    fn long_members_sharing_an_excerpt_prefix_stay_separate() {
        let store = InsightsStore::open_in_memory().unwrap();
        let shared = "x".repeat(300);
        let (one, two) = (format!("{shared} one"), format!("{shared} two"));
        upsert_mined(&store, &mined(&one, &[&one], Destination::Skill)).unwrap();
        let id = store.list_proposals(None).unwrap()[0].id;
        store
            .set_proposal_status(id, ProposalStatus::Rejected, None)
            .unwrap();
        upsert_mined(&store, &mined(&two, &[&two], Destination::Skill)).unwrap();
        let rows = store.list_proposals(None).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].status, ProposalStatus::Proposed);
    }

    #[test]
    fn accepting_same_slug_and_fingerprint_prefix_keeps_both_memories() {
        let store = InsightsStore::open_in_memory().unwrap();
        let mut ids = Vec::new();
        for tail in ["a", "b"] {
            let mut c = candidate("memory_fact", "demo", tail, "Use pnpm", tail, 1, 1, vec![]);
            c.fingerprint = format!("{}{tail}", "f".repeat(12));
            store.upsert_proposal_candidate(&c).unwrap();
            ids.push(store.list_proposals(None).unwrap().last().unwrap().id);
        }
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::memory::schema::migrate(&mut conn).unwrap();
        for id in ids {
            accept(&store, &conn, id).unwrap();
        }
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM observations WHERE title='Use pnpm'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
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

    fn seed_rule_at(store: &InsightsStore, target: &Path, content: &str) -> i64 {
        let mut c = candidate("rule", "demo", content, "Prefer rg", content, 1, 1, vec![]);
        c.target_path = Some(target.display().to_string());
        store.upsert_proposal_candidate(&c).unwrap();
        store.list_proposals(None).unwrap()[0].id
    }

    #[test]
    fn apply_without_yes_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("AGENTS.md");
        std::fs::write(&target, "# Agent\n\nkeep me\n").unwrap();
        let store = InsightsStore::open_in_memory().unwrap();
        let id = seed_rule_at(&store, &target, "always use rg");
        let before = std::fs::read_to_string(&target).unwrap();
        let diff = apply(&store, id, false, false, dir.path()).unwrap();
        assert!(diff.contains("## Learned") || diff.contains("always use rg"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), before);
        assert_eq!(
            store.get_proposal(id).unwrap().unwrap().status,
            ProposalStatus::Proposed
        );
        assert!(
            store
                .get_proposal(id)
                .unwrap()
                .unwrap()
                .applied_hash
                .is_none()
        );
    }

    #[test]
    fn apply_refuses_when_target_changed_since_preview() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("AGENTS.md");
        std::fs::write(&target, "# Agent\n").unwrap();
        let before = std::fs::read_to_string(&target).unwrap();
        let preview = sha256_hex(before.as_bytes());
        let after = merge_learned(&before, "never skip tests");
        std::fs::write(&target, "# Agent\n\nchanged underneath\n").unwrap();
        let err = recheck_and_write(&target, &preview, &after).unwrap_err();
        assert!(err.to_string().contains("changed since preview"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "# Agent\n\nchanged underneath\n"
        );
    }

    #[test]
    fn apply_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        std::fs::write(&target, "# Claude\n").unwrap();
        let store = InsightsStore::open_in_memory().unwrap();
        let id = seed_rule_at(&store, &target, "always run cargo test");
        apply(&store, id, true, false, dir.path()).unwrap();
        let once = std::fs::read_to_string(&target).unwrap();
        let hash = store
            .get_proposal(id)
            .unwrap()
            .unwrap()
            .applied_hash
            .clone()
            .unwrap();
        apply(&store, id, true, false, dir.path()).unwrap();
        let twice = std::fs::read_to_string(&target).unwrap();
        assert_eq!(once, twice);
        assert_eq!(once.matches("always run cargo test").count(), 1);
        assert_eq!(
            store.get_proposal(id).unwrap().unwrap().status,
            ProposalStatus::Applied
        );
        assert_eq!(
            store
                .get_proposal(id)
                .unwrap()
                .unwrap()
                .applied_hash
                .as_deref(),
            Some(hash.as_str())
        );
    }

    fn seed_kind(store: &InsightsStore, kind: &str) -> i64 {
        let c = candidate(kind, "demo", "k", "T", "some text", 1, 1, vec![]);
        store.upsert_proposal_candidate(&c).unwrap();
        store.list_proposals(None).unwrap()[0].id
    }

    #[test]
    fn apply_refuses_memory_fact_kind() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("AGENTS.md");
        std::fs::write(&target, "# Agent\n").unwrap();
        let store = InsightsStore::open_in_memory().unwrap();
        let id = seed_kind(&store, "memory_fact");
        let err = apply(&store, id, true, false, dir.path()).unwrap_err();
        assert!(err.to_string().contains("reflect accept"), "{err}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "# Agent\n");
        let row = store.get_proposal(id).unwrap().unwrap();
        assert_eq!(row.status, ProposalStatus::Proposed);
        assert!(row.applied_hash.is_none());
    }

    #[test]
    fn apply_refuses_command_and_none_kinds() {
        for kind in ["command", "none", "bogus"] {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("AGENTS.md");
            std::fs::write(&target, "# Agent\n").unwrap();
            let store = InsightsStore::open_in_memory().unwrap();
            let id = seed_kind(&store, kind);
            let err = apply(&store, id, true, false, dir.path()).unwrap_err();
            assert!(err.to_string().contains("has no applyable target"), "{err}");
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "# Agent\n");
            let row = store.get_proposal(id).unwrap().unwrap();
            assert_eq!(row.status, ProposalStatus::Proposed);
            assert!(row.applied_hash.is_none());
        }
    }

    #[test]
    fn apply_preserves_unrelated_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("AGENTS.md");
        std::fs::write(&target, "# Agent\n\n## Setup\n\nkeep this section\n").unwrap();
        let store = InsightsStore::open_in_memory().unwrap();
        let id = seed_rule_at(&store, &target, "prefer tempfile for fixtures");
        apply(&store, id, true, false, dir.path()).unwrap();
        let body = std::fs::read_to_string(&target).unwrap();
        assert!(body.contains("## Setup"));
        assert!(body.contains("keep this section"));
        assert!(body.contains("## Learned"));
        assert!(body.contains("prefer tempfile for fixtures"));
    }

    #[test]
    fn apply_inserts_inside_a_learned_section_that_is_not_last() {
        let existing = "# A\n\n## Learned\n\nold rule\n\n## Setup\n\nkeep\n";
        assert_eq!(
            merge_learned(existing, "new rule"),
            "# A\n\n## Learned\n\nold rule\n\nnew rule\n\n## Setup\n\nkeep\n"
        );
        // Text outside the section does not count as already applied.
        let outside = "## Setup\n\nnew rule\n\n## Learned\n\nold rule\n";
        assert!(merge_learned(outside, "new rule").ends_with("old rule\n\nnew rule\n"));
    }

    #[test]
    fn apply_refuses_rejected_proposal() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("AGENTS.md");
        let store = InsightsStore::open_in_memory().unwrap();
        let id = seed_rule_at(&store, &target, "never apply me");
        store
            .set_proposal_status(id, ProposalStatus::Rejected, None)
            .unwrap();
        let err = apply(&store, id, true, false, dir.path()).unwrap_err();
        assert!(err.to_string().contains("rejected"), "{err}");
        assert!(!target.exists());
        assert_eq!(
            store.get_proposal(id).unwrap().unwrap().status,
            ProposalStatus::Rejected
        );
    }

    #[test]
    fn apply_never_overwrites_a_different_skill_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(".claude/skills/demo/SKILL.md");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "hand written\n").unwrap();
        let store = InsightsStore::open_in_memory().unwrap();
        let id = seed_rule_at(&store, &target, "mined skill");
        let err = apply(&store, id, true, false, dir.path()).unwrap_err();
        assert!(err.to_string().contains("refusing to overwrite"), "{err}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hand written\n");
    }

    #[test]
    fn skill_with_no_ascii_title_still_gets_its_own_directory() {
        let dir = tempfile::tempdir().unwrap();
        let c = candidate("skill", "demo", "k", "日本語", "body", 1, 1, vec![]);
        let path = resolve_target(&c, false, dir.path()).unwrap();
        let expected = format!("skill-{}", &c.fingerprint[..12]);
        assert_eq!(
            path,
            dir.path()
                .join(".claude/skills")
                .join(expected)
                .join("SKILL.md")
        );
    }

    #[test]
    fn source_log_guard_holds_for_windows_separators() {
        assert!(is_source_log_path(Path::new(
            r"C:\Users\x\.claude\projects\a\notes.md"
        )));
        assert!(is_source_log_path(Path::new(r"C:\Users\x\.codex\notes.md")));
        assert!(is_source_log_path(Path::new(
            "/home/x/.claude/projects/a/notes.md"
        )));
        assert!(!is_source_log_path(Path::new(r"C:\repo\AGENTS.md")));
    }

    #[test]
    fn global_target_requires_flag() {
        let home = dirs::home_dir().expect("HOME");
        let target = home.join(".claude/rules/insights.md");
        let store = InsightsStore::open_in_memory().unwrap();
        let id = seed_rule_at(&store, &target, "global rule text");
        let err = apply(&store, id, false, false, Path::new("/tmp")).unwrap_err();
        assert!(
            err.to_string().contains("--global"),
            "expected --global requirement, got {err}"
        );
        assert!(apply(&store, id, false, true, Path::new("/tmp")).is_ok());
    }
}
