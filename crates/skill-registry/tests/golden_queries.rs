//! Golden-query recall: 20 realistic task phrasings against a fixture corpus.
//! This is the BM25-sufficiency gate from the design spec: if hit@3 drops
//! below 85%, the v2 embeddings feature is justified.

use agentflare_skill_registry::db::{open_in_memory, rebuild};
use agentflare_skill_registry::search::{MatchMode, search};
use agentflare_skill_registry::sources::{Source, SourceKind, scan_sources};
use std::fs;
use std::path::Path;

const FIXTURES: &[(&str, &str)] = &[
    (
        "live",
        "Use when the user asks about running sessions, agent status, what needs attention — e.g. 'what's running', 'live status', 'anything stuck'",
    ),
    (
        "cv-usage",
        "Use when the user asks about usage analytics, statistics, token usage, or cost summary — e.g. 'usage stats', 'how many sessions this week', 'cost report'",
    ),
    (
        "win-cleanup",
        "Use when the user asks to free disk space, clean up the Windows system, says disk is full, or wants a disk usage report",
    ),
    (
        "code-review",
        "Review the current diff for correctness bugs and reuse, simplification, efficiency cleanups",
    ),
    (
        "deep-research",
        "Deep research harness — fan-out web searches, fetch sources, verify claims, synthesize a cited report",
    ),
    (
        "short-skill",
        "Use when the user wants a compressed shorthand version of an installed skill, complains a skill is bloated, verbose, or token-heavy",
    ),
];

const GOLDEN: &[(&str, &str)] = &[
    ("what's running right now", "live"),
    ("are my agents stuck", "live"),
    ("check on background sessions", "live"),
    ("how much did I spend on tokens this week", "cv-usage"),
    ("usage statistics", "cv-usage"),
    ("session count report", "cv-usage"),
    ("my disk is full", "win-cleanup"),
    ("free up space on windows", "win-cleanup"),
    ("clean temp files", "win-cleanup"),
    ("review my diff for bugs", "code-review"),
    ("check this code for correctness", "code-review"),
    ("find efficiency cleanups", "code-review"),
    ("research this topic with cited sources", "deep-research"),
    ("fan out web searches and verify claims", "deep-research"),
    ("write me a fact checked report", "deep-research"),
    ("this skill is too verbose", "short-skill"),
    ("compress a bloated skill", "short-skill"),
    ("make a shorthand version of a skill", "short-skill"),
    ("skill is token heavy", "short-skill"),
    ("what needs my attention", "live"),
];

fn write_fixture(root: &Path) {
    for (name, desc) in FIXTURES {
        let d = root.join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: \"{desc}\"\n---\nbody\n"),
        )
        .unwrap();
    }
}

#[test]
fn golden_queries_hit_at_3_is_at_least_85_percent() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path());
    let out = scan_sources(&[Source {
        id: "fixture".into(),
        kind: SourceKind::FlatDir(tmp.path().to_path_buf()),
    }]);
    assert_eq!(out.entries.len(), FIXTURES.len());
    let mut conn = open_in_memory().unwrap();
    rebuild(&mut conn, &out.entries).unwrap();

    let mut hits = 0usize;
    let mut misses = Vec::new();
    for (query, expected) in GOLDEN {
        // Agent behavior: try All, retry Any — mirror it here.
        let mut r = search(&conn, query, 3, MatchMode::All).unwrap();
        if r.is_empty() {
            r = search(&conn, query, 3, MatchMode::Any).unwrap();
        }
        if r.iter().any(|h| h.name == *expected) {
            hits += 1;
        } else {
            misses.push(format!(
                "{query:?} -> wanted {expected}, got {:?}",
                r.iter().map(|h| h.name.clone()).collect::<Vec<_>>()
            ));
        }
    }
    let rate = hits as f64 / GOLDEN.len() as f64;
    println!(
        "hit@3 = {rate:.2} ({} / {} queries matched)",
        hits,
        GOLDEN.len()
    );
    assert!(
        rate >= 0.85,
        "hit@3 {rate:.2} below gate; misses:\n{}",
        misses.join("\n")
    );
}
