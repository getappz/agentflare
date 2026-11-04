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

#[derive(Debug, Default, PartialEq)]
struct Score {
    n: usize,
    p_at_k: f64,
    hit: f64,
}

fn seed(extra: &[Obs]) -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    schema::migrate(&mut conn).unwrap();
    for (_, ty, title, content) in UNIVERSE.iter().chain(extra) {
        observations::save(
            &conn,
            None,
            ty,
            title,
            content,
            None,
            Some(PROJECT),
            None,
            None,
        )
        .unwrap();
    }
    conn
}

/// Embedding is off (`|_| None`) so scores are model-independent.
fn run(extra: &[Obs]) -> Vec<(&'static str, Score)> {
    let conn = seed(extra);
    let mut out: Vec<(&'static str, Score)> = Vec::new();
    for (dim, query, relevant) in SCENARIOS {
        let hits = search::search_hybrid(&conn, query, Some(PROJECT), None, K, |_| None).unwrap();
        let found = hits
            .iter()
            .filter(|o| relevant.contains(&o.title.as_str()))
            .count();
        if out.last().is_none_or(|(d, _)| d != dim) {
            out.push((dim, Score::default()));
        }
        let s = &mut out.last_mut().unwrap().1;
        s.n += 1;
        s.p_at_k += found as f64 / K as f64;
        s.hit += (found > 0) as u8 as f64;
    }
    for (_, s) in &mut out {
        s.p_at_k /= s.n as f64;
        s.hit /= s.n as f64;
    }
    out
}

fn print_table(rows: &[(&str, Score)]) {
    println!("memory bench (embedding: off, k={K})");
    println!("{:<8} {:>3} {:>6} {:>6}", "dim", "n", "P@k", "hit");
    for (d, s) in rows {
        println!("{d:<8} {:>3} {:>6.3} {:>6.3}", s.n, s.p_at_k, s.hit);
    }
}

#[test]
fn bench_is_deterministic_and_prints_table() {
    let a = run(&[]);
    print_table(&a);
    assert_eq!(a, run(&[]));
    assert!(
        a.iter().all(|(_, s)| s.hit == 1.0),
        "baseline should hit: {a:?}"
    );
}

#[test]
fn outranking_distractors_lower_score() {
    let base = run(&[]);
    // Title-stuffed distractors (title bm25 weight 3) crowd out the answer.
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
    let worse = run(d);
    let bug = |r: &[(&str, Score)]| r.iter().find(|(n, _)| *n == "bug").unwrap().1.p_at_k;
    assert!(
        bug(&worse) < bug(&base),
        "{} !< {}",
        bug(&worse),
        bug(&base)
    );
}
