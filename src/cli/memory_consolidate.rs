//! `agentflare memory consolidate`: offline, deterministic (no LLM) pass that
//! clusters similar observations and proposes one evidence-backed belief per
//! cluster through the Insights proposals ledger. Review with
//! `insights reflect list|show|accept|reject|snooze`; accepting a belief
//! (see `insights_reflect::accept`) saves it as an observation and links every
//! evidence observation to it with a `supports` relation.
use std::collections::{BTreeMap, HashSet};

use flare_insights::evidence::EvidenceRef;
use flare_insights::proposals::fingerprint;
use flare_insights::store::InsightsStore;

use super::insights_reflect::candidate;
use crate::memory::observations::{self, Observation};
use crate::memory::relations;

/// Relation from an evidence observation to the belief built from it.
/// (`evidence_for` is reserved for workflow-gated promotion.)
pub(crate) const SUPPORTS: &str = "supports";
const HEADER: &str = "evidence:";
const KIND: &str = "memory_fact";
// flare-code: O(n^2) pairwise scan over the newest CAP observations; switch to
// an FTS/minhash candidate index if memory grows past a few thousand rows.
const CAP: usize = 500;
const MAX_CLUSTER: usize = 12;
const STOP: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "not", "are", "was", "but", "its", "use",
    "has", "have", "into", "when", "then", "than", "can", "will", "via", "per", "all", "any",
];

fn tokens(o: &Observation) -> HashSet<String> {
    format!("{} {}", o.title, o.content)
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|t| t.len() >= 3 && !STOP.contains(&t.as_str()))
        .collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    let inter = a.intersection(b).count();
    let union = a.len() + b.len() - inter;
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

/// Index groups (single-link) of observations whose token sets overlap at
/// least `threshold`; groups smaller than `min_size` are dropped.
fn clusters(obs: &[Observation], threshold: f64, min_size: usize) -> Vec<Vec<usize>> {
    let toks: Vec<_> = obs.iter().map(tokens).collect();
    let mut parent: Vec<usize> = (0..obs.len()).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..obs.len() {
        for j in i + 1..obs.len() {
            if jaccard(&toks[i], &toks[j]) >= threshold {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                parent[ri] = rj;
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..obs.len() {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    // Single-link chains unrelated notes together at low thresholds (a real
    // brain.db at 0.15 produced a 25-row git/CI/worktree blob), so oversized
    // groups are dropped rather than proposed as one belief.
    groups
        .into_values()
        .filter(|g| (min_size..=MAX_CLUSTER).contains(&g.len()))
        .collect()
}

/// Belief title/content for one cluster: the most-repeated (then newest)
/// member's text, a machine-readable evidence header, and the other titles.
fn belief(members: &[&Observation]) -> (String, String) {
    let rep = members
        .iter()
        .max_by_key(|o| (o.duplicate_count, o.updated_at.clone()))
        .expect("non-empty cluster");
    let mut ids: Vec<i64> = members.iter().map(|o| o.id).collect();
    ids.sort_unstable();
    let ids = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let others = members
        .iter()
        .filter(|o| o.id != rep.id)
        .map(|o| o.title.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    let content = format!(
        "{HEADER} n={} ids={ids}\n\n{}\n\nAlso seen in: {others}",
        members.len(),
        rep.content
    );
    (rep.title.clone(), content)
}

/// Observation ids from a consolidated belief's header, empty for any other
/// proposal content.
pub(crate) fn evidence_ids(content: &str) -> Vec<i64> {
    content
        .lines()
        .next()
        .filter(|l| l.starts_with(HEADER))
        .and_then(|l| l.split("ids=").nth(1))
        .map(|s| s.split(',').filter_map(|n| n.trim().parse().ok()).collect())
        .unwrap_or_default()
}

/// Link each evidence observation to `belief_id`; returns links written.
pub(crate) fn link_evidence(
    conn: &rusqlite::Connection,
    belief_id: i64,
    ids: &[i64],
) -> rusqlite::Result<usize> {
    let mut n = 0;
    for &id in ids.iter().filter(|&&id| id != belief_id) {
        relations::create(
            conn,
            id,
            belief_id,
            SUPPORTS,
            None,
            Some("consolidated belief"),
            None,
            None,
        )?;
        n += 1;
    }
    Ok(n)
}

/// Clusters of observations as `(project, members)`, one project at a time.
fn grouped(
    conn: &rusqlite::Connection,
    project: Option<&str>,
    threshold: f64,
    min_size: usize,
) -> rusqlite::Result<Vec<(String, Vec<Observation>)>> {
    let mut by_project: BTreeMap<String, Vec<Observation>> = BTreeMap::new();
    for o in observations::list_recent(conn, project, None, CAP)? {
        // Skip already-consolidated beliefs and personal-scope rows.
        let is_belief = o
            .topic_key
            .as_deref()
            .is_some_and(|k| k.starts_with("insights/"));
        if !is_belief && o.scope != "personal" {
            by_project
                .entry(o.project.clone().unwrap_or_default())
                .or_default()
                .push(o);
        }
    }
    Ok(by_project
        .into_iter()
        .flat_map(|(proj, obs)| {
            clusters(&obs, threshold, min_size)
                .into_iter()
                .map(|g| (proj.clone(), g.iter().map(|&i| obs[i].clone()).collect()))
                .collect::<Vec<_>>()
        })
        .collect())
}

/// What `scan` would propose, one line per cluster plus its members; writes nothing.
pub(crate) fn preview(
    conn: &rusqlite::Connection,
    project: Option<&str>,
    threshold: f64,
    min_size: usize,
) -> rusqlite::Result<String> {
    let mut out = String::new();
    for (proj, members) in grouped(conn, project, threshold, min_size)? {
        let refs: Vec<&Observation> = members.iter().collect();
        out.push_str(&format!(
            "[{proj}] n={} {}\n",
            members.len(),
            belief(&refs).0
        ));
        for m in &members {
            out.push_str(&format!("    #{} {}\n", m.id, m.title));
        }
    }
    Ok(out)
}

/// Cluster observations (per project) and upsert one ledger candidate per
/// cluster. Returns the number of clusters proposed. Idempotent: a cluster
/// that gains a member keeps its fingerprint through member-id aliases.
pub(crate) fn scan(
    store: &InsightsStore,
    conn: &rusqlite::Connection,
    project: Option<&str>,
    threshold: f64,
    min_size: usize,
) -> Result<usize, Box<dyn std::error::Error>> {
    let mut proposed = 0;
    for (proj, obs) in &grouped(conn, project, threshold, min_size)? {
        let members: Vec<&Observation> = obs.iter().collect();
        let (title, content) = belief(&members);
        let aliases: Vec<String> = members
            .iter()
            .map(|o| fingerprint("belief", proj, &format!("obs:{}", o.id)))
            .collect();
        let evidence = members
            .iter()
            .map(|o| EvidenceRef {
                source: "memory".into(),
                session_id: format!("obs:{}", o.id),
                turn_seq: None,
                tool_call_id: None,
                at: None,
                excerpt: Some(o.title.clone()),
            })
            .collect();
        let mut c = candidate(
            KIND,
            proj,
            &aliases[0],
            &title,
            &content,
            0,
            members.len() as i64,
            evidence,
        );
        if let Some(existing) = store.proposal_for_aliases(&aliases)? {
            c.fingerprint = existing;
        }
        store.upsert_proposal_candidate(&c)?;
        store.add_proposal_aliases(&c.fingerprint, &aliases)?;
        proposed += 1;
    }
    Ok(proposed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::schema;
    use flare_insights::proposals::ProposalStatus;

    fn db() -> rusqlite::Connection {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        schema::migrate(&mut conn).unwrap();
        conn
    }

    fn save(conn: &rusqlite::Connection, title: &str, content: &str, project: &str) -> i64 {
        match observations::save(
            conn,
            None,
            "learning",
            title,
            content,
            None,
            Some(project),
            None,
            None,
        )
        .unwrap()
        {
            observations::SaveOutcome::Created(id)
            | observations::SaveOutcome::Updated(id)
            | observations::SaveOutcome::Duplicate(id) => id,
        }
    }

    fn seed_cluster(conn: &rusqlite::Connection, project: &str) -> Vec<i64> {
        [
            "build cache dir shared across worktrees breaks builds set BUILD_CACHE_DIR task local",
            "worktree builds clobber shared build cache dir use task local BUILD_CACHE_DIR",
            "never share build cache dir between worktrees; set task local BUILD_CACHE_DIR",
        ]
        .iter()
        .enumerate()
        .map(|(i, c)| save(conn, &format!("build cache gotcha {i}"), c, project))
        .collect()
    }

    #[test]
    fn similar_observations_become_one_cited_candidate_and_unrelated_is_left_out() {
        let conn = db();
        let store = InsightsStore::open_in_memory().unwrap();
        let ids = seed_cluster(&conn, "p");
        save(
            &conn,
            "celld reference",
            "audit of denoland celld harness semantics",
            "p",
        );
        assert_eq!(scan(&store, &conn, Some("p"), 0.4, 3).unwrap(), 1);
        let p = &store.list_proposals(None).unwrap()[0];
        assert_eq!(p.kind, "memory_fact");
        let mut found = evidence_ids(&p.proposed_content);
        found.sort_unstable();
        assert_eq!(found, ids);
    }

    #[test]
    fn oversized_chain_clusters_are_not_proposed() {
        let conn = db();
        for i in 0..=MAX_CLUSTER {
            save(
                &conn,
                &format!("target dir note {i}"),
                "build cache dir worktrees task local BUILD_CACHE_DIR gotcha",
                "p",
            );
        }
        assert_eq!(preview(&conn, Some("p"), 0.4, 3).unwrap(), "");
    }

    #[test]
    fn preview_lists_clusters_with_members_and_writes_nothing() {
        let conn = db();
        let ids = seed_cluster(&conn, "p");
        save(
            &conn,
            "celld reference",
            "audit of denoland celld harness semantics",
            "p",
        );
        let out = preview(&conn, Some("p"), 0.4, 3).unwrap();
        assert!(out.starts_with("[p] n=3 "), "{out}");
        for id in ids {
            assert!(out.contains(&format!("#{id} ")), "{out}");
        }
        assert!(!out.contains("celld reference"), "{out}");
    }

    #[test]
    fn rescan_is_idempotent_and_a_new_member_keeps_the_same_proposal() {
        let conn = db();
        let store = InsightsStore::open_in_memory().unwrap();
        seed_cluster(&conn, "p");
        scan(&store, &conn, Some("p"), 0.4, 3).unwrap();
        scan(&store, &conn, Some("p"), 0.4, 3).unwrap();
        assert_eq!(store.list_proposals(None).unwrap().len(), 1);
        save(
            &conn,
            "build cache gotcha 3",
            "shared build cache dir across worktrees: set task local BUILD_CACHE_DIR",
            "p",
        );
        scan(&store, &conn, Some("p"), 0.4, 3).unwrap();
        let all = store.list_proposals(None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(evidence_ids(&all[0].proposed_content).len(), 4);
    }

    #[test]
    fn rejected_cluster_stays_rejected_and_projects_never_mix() {
        let conn = db();
        let store = InsightsStore::open_in_memory().unwrap();
        seed_cluster(&conn, "p");
        scan(&store, &conn, None, 0.4, 3).unwrap();
        let id = store.list_proposals(None).unwrap()[0].id;
        store
            .set_proposal_status(id, ProposalStatus::Rejected, None)
            .unwrap();
        scan(&store, &conn, None, 0.4, 3).unwrap();
        assert_eq!(
            store.get_proposal(id).unwrap().unwrap().status,
            ProposalStatus::Rejected
        );
        // Two similar rows in different projects are not a cluster.
        let conn2 = db();
        let store2 = InsightsStore::open_in_memory().unwrap();
        save(
            &conn2,
            "a",
            "build cache dir task local BUILD_CACHE_DIR",
            "x",
        );
        save(
            &conn2,
            "b",
            "build cache dir task local BUILD_CACHE_DIR",
            "y",
        );
        save(
            &conn2,
            "c",
            "build cache dir task local BUILD_CACHE_DIR",
            "z",
        );
        assert_eq!(scan(&store2, &conn2, None, 0.4, 3).unwrap(), 0);
    }

    #[test]
    fn accept_saves_belief_links_evidence_and_refines_on_reaccept() {
        let conn = db();
        let store = InsightsStore::open_in_memory().unwrap();
        let ids = seed_cluster(&conn, "p");
        scan(&store, &conn, Some("p"), 0.4, 3).unwrap();
        let pid = store.list_proposals(None).unwrap()[0].id;
        super::super::insights_reflect::accept(&store, &conn, pid).unwrap();
        let belief_id: i64 = conn
            .query_row(
                "SELECT id FROM observations WHERE topic_key LIKE 'insights/memory_fact/%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let linked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_relations WHERE target_id=?1 AND relation='supports'",
                [belief_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(linked as usize, ids.len());
        // A fourth member arrives; rescan + re-accept refines the same belief row.
        save(
            &conn,
            "build cache gotcha 3",
            "shared build cache dir across worktrees: set task local BUILD_CACHE_DIR",
            "p",
        );
        scan(&store, &conn, Some("p"), 0.4, 3).unwrap();
        super::super::insights_reflect::accept(&store, &conn, pid).unwrap();
        let (n, rev): (i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM memory_relations WHERE target_id=o.id AND relation='supports'), revision_count
                 FROM observations o WHERE topic_key LIKE 'insights/memory_fact/%'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(n, 4);
        assert!(rev >= 1, "belief should be refined, not duplicated");
    }
}
