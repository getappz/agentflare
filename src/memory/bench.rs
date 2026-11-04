//! Deterministic memory-recall benchmark (item #383). Seeds a temp DB from a
//! tiny synthetic universe, queries through `search_hybrid` (the hook path)
//! and scores precision@k + hit rate per dimension. No LLM, no timing.
//! Run: `cargo test --bin agentflare memory::bench -- --nocapture`.
#![cfg(test)]

use super::{observations, schema, search};
use rusqlite::Connection;

const K: usize = 3;
const PROJECT: &str = "bench";

/// (dimension, type, title, content). Titles are unique ids.
type Obs = (&'static str, &'static str, &'static str, &'static str);

const UNIVERSE: &[Obs] = &[
    (
        "pref",
        "preference",
        "pref tabs",
        "user prefers tabs over spaces in rust code",
    ),
    (
        "pref",
        "preference",
        "pref spaces old",
        "user once wanted spaces in python formatting",
    ),
    (
        "pref",
        "preference",
        "pref commit style",
        "commit messages short imperative no trailers",
    ),
    (
        "topo",
        "architecture",
        "topo daemon",
        "the daemon spawns jobs through the scheduler crate",
    ),
    (
        "topo",
        "architecture",
        "topo bridge",
        "github bridge polls issues and feeds the scheduler",
    ),
    (
        "topo",
        "architecture",
        "topo sidecar",
        "browser sidecar runs playwright outside the daemon",
    ),
    (
        "time",
        "decision",
        "time sqlite switch",
        "2026-03 migrated storage from json files to sqlite",
    ),
    (
        "time",
        "decision",
        "time fts added",
        "2026-05 added fts5 search after sqlite migration",
    ),
    (
        "time",
        "decision",
        "time json legacy",
        "json files were the original storage format",
    ),
    (
        "bug",
        "bugfix",
        "bug fts collision",
        "fts query failed because limit and project shared placeholder two",
    ),
    (
        "bug",
        "bugfix",
        "bug lock stale",
        "stale lock file blocked worktree claim until removed",
    ),
    (
        "bug",
        "bugfix",
        "bug unrelated",
        "css button misaligned on safari",
    ),
    (
        "combo",
        "decision",
        "combo retry policy",
        "scheduler retry uses exponential backoff after bridge rate limit",
    ),
    (
        "combo",
        "bugfix",
        "combo retry bug",
        "retry loop spun hot when bridge returned rate limit headers",
    ),
    // Conflicting: same topic, superseded claim.
    (
        "pref",
        "preference",
        "pref spaces rust",
        "superseded: user prefers spaces over tabs in rust code",
    ),
    (
        "combo",
        "note",
        "combo noise",
        "weekly standup notes about rate of hiring",
    ),
];

/// (dimension, query, relevant titles)
const SCENARIOS: &[(&str, &str, &[&str])] = &[
    ("pref", "tabs rust code", &["pref tabs"]),
    ("pref", "commit messages", &["pref commit style"]),
    ("topo", "scheduler daemon jobs", &["topo daemon"]),
    ("topo", "playwright sidecar", &["topo sidecar"]),
    ("time", "migrated storage sqlite", &["time sqlite switch"]),
    ("time", "fts5 search added", &["time fts added"]),
    ("bug", "fts query placeholder", &["bug fts collision"]),
    ("bug", "stale lock claim", &["bug lock stale"]),
    (
        "combo",
        "bridge rate limit retry",
        &["combo retry policy", "combo retry bug"],
    ),
];

const OTHER_PROJECT: Obs = (
    "pref",
    "preference",
    "other tabs",
    "tabs rust code tabs rust code",
);

/// Deterministic fake embedder (no ONNX): one axis per keyword.
fn fake_embed(text: &str) -> Option<Vec<f32>> {
    let t = text.to_lowercase();
    Some(
        [
            "tabs",
            "scheduler",
            "sqlite",
            "fts",
            "retry",
            "playwright",
            "lock",
        ]
        .iter()
        .map(|w| t.contains(w) as u8 as f32)
        .collect(),
    )
}

#[derive(Debug, Default, PartialEq)]
struct Score {
    n: usize,
    p_at_k: f64,
    hit: f64,
    hit1: f64,
    mrr: f64,
}

fn seed(extra: &[Obs]) -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    schema::migrate(&mut conn).unwrap();
    for (_, ty, title, content) in UNIVERSE.iter().chain(extra) {
        let project = if *title == OTHER_PROJECT.2 {
            "other"
        } else {
            PROJECT
        };
        observations::save(
            &conn,
            None,
            ty,
            title,
            content,
            None,
            Some(project),
            None,
            None,
        )
        .unwrap();
    }
    conn
}

/// Mirrors `memory_nudge::recall_with`: project=None, limit TOP_K*3, then
/// the project/global scope filter, then top K. (The session dedup ledger
/// is a process-global file and is deliberately not exercised.)
fn run(embed: Option<fn(&str) -> Option<Vec<f32>>>, extra: &[Obs]) -> Vec<(&'static str, Score)> {
    let conn = seed(extra);
    let embed = embed.unwrap_or(|_| None);
    let mut out: Vec<(&'static str, Score)> = Vec::new();
    for (dim, query, relevant) in SCENARIOS {
        let hits: Vec<_> = search::search_hybrid(&conn, query, None, None, K * 3, embed)
            .unwrap()
            .into_iter()
            .filter(|o| o.project.as_deref().is_none_or(|p| p == PROJECT))
            .take(K)
            .collect();
        let rel = |o: &observations::Observation| relevant.contains(&o.title.as_str());
        let found = hits.iter().filter(|o| rel(o)).count();
        if out.last().is_none_or(|(d, _)| d != dim) {
            out.push((dim, Score::default()));
        }
        let s = &mut out.last_mut().unwrap().1;
        s.n += 1;
        s.p_at_k += found as f64 / K as f64;
        s.hit += (found > 0) as u8 as f64;
        s.hit1 += hits.first().is_some_and(rel) as u8 as f64;
        s.mrr += hits
            .iter()
            .position(rel)
            .map_or(0.0, |r| 1.0 / (r + 1) as f64);
    }
    for (_, s) in &mut out {
        let n = s.n as f64;
        s.p_at_k /= n;
        s.hit /= n;
        s.hit1 /= n;
        s.mrr /= n;
    }
    out
}

fn print_table(mode: &str, rows: &[(&str, Score)]) {
    println!("memory bench (embedding: {mode}, k={K})");
    println!(
        "{:<8} {:>3} {:>6} {:>6} {:>6} {:>6}",
        "dim", "n", "P@k", "hit", "hit@1", "MRR"
    );
    for (d, s) in rows {
        println!(
            "{d:<8} {:>3} {:>6.3} {:>6.3} {:>6.3} {:>6.3}",
            s.n, s.p_at_k, s.hit, s.hit1, s.mrr
        );
    }
}

#[test]
fn bench_is_deterministic_and_prints_table() {
    for (mode, e) in [("off", None), ("fake", Some(fake_embed as fn(&str) -> _))] {
        let a = run(e, &[OTHER_PROJECT]);
        print_table(mode, &a);
        assert_eq!(a, run(e, &[OTHER_PROJECT]));
        assert!(
            a.iter().all(|(_, s)| s.hit == 1.0),
            "{mode} should hit: {a:?}"
        );
    }
}

#[test]
fn outranking_distractors_lower_score() {
    let mrr = |r: &[(&str, Score)]| r.iter().find(|(n, _)| *n == "bug").unwrap().1.mrr;
    let base = run(None, &[]);
    // Title-stuffed distractors (title bm25 weight 3) outrank the answer.
    let d: &[Obs] = &[
        (
            "bug",
            "bugfix",
            "fts query placeholder a",
            "fts query placeholder",
        ),
        (
            "bug",
            "bugfix",
            "fts query placeholder b",
            "fts query placeholder",
        ),
        (
            "bug",
            "bugfix",
            "fts query placeholder c",
            "fts query placeholder",
        ),
    ];
    let worse = run(None, d);
    assert!(
        mrr(&worse) < mrr(&base),
        "{} !< {}",
        mrr(&worse),
        mrr(&base)
    );
}
