//! `agentflare memory bench --cases <file.jsonl>`: scores recall on the real
//! brain.db. One JSON object per line: {"query": "...", "expect": ["topic_key or title", ...]}.
//! Compares lexical-only against hybrid (embeddings, when the build has them).
//! The synthetic `bench.rs` is saturated (every score 1.0), so it only guards
//! regressions; this one measures the data we actually have.
use super::{observations::Observation, search};
use rusqlite::Connection;
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    query: String,
    expect: Vec<String>,
}

type Embed = fn(&str) -> Option<Vec<f32>>;

fn is_hit(o: &Observation, expect: &[String]) -> bool {
    expect
        .iter()
        .any(|e| o.topic_key.as_deref() == Some(e) || o.title == *e)
}

/// Returns "mode  n  hit@k  hit@1  MRR" for one embedder.
fn score(
    conn: &Connection,
    cases: &[Case],
    project: Option<&str>,
    k: usize,
    embed: Embed,
) -> Result<(f64, f64, f64), rusqlite::Error> {
    let (mut hit, mut hit1, mut mrr) = (0.0, 0.0, 0.0);
    for c in cases {
        let rank = search::search_hybrid(conn, &c.query, project, None, k, embed)?
            .iter()
            .position(|o| is_hit(o, &c.expect));
        hit += rank.is_some() as u8 as f64;
        hit1 += (rank == Some(0)) as u8 as f64;
        mrr += rank.map_or(0.0, |r| 1.0 / (r + 1) as f64);
    }
    let n = cases.len().max(1) as f64;
    Ok((hit / n, hit1 / n, mrr / n))
}

pub fn run(
    conn: &Connection,
    cases_jsonl: &str,
    project: Option<&str>,
    k: usize,
) -> Result<String, String> {
    if k > 50 {
        return Err("k must not exceed 50 (search caps results at 50)".into());
    }
    let cases: Vec<Case> = cases_jsonl
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| serde_json::from_str(l).map_err(|e| format!("case line {}: {e}", i + 1)))
        .collect::<Result<_, _>>()?;
    if cases.is_empty() {
        return Err("no cases".into());
    }
    let mut out = format!(
        "memory recall bench: {} cases, k={k}\n{:<10} {:>7} {:>7} {:>7}\n",
        cases.len(),
        "mode",
        "hit@k",
        "hit@1",
        "MRR"
    );
    let mut modes: Vec<(&str, Embed)> = vec![("lexical", |_| None)];
    if super::engine::model_name().is_some() {
        modes.push(("hybrid", super::engine::embed_query));
    } else {
        out.push_str("(no embedding engine in this build: hybrid row skipped)\n");
    }
    for (name, embed) in modes {
        let (hit, hit1, mrr) = score(conn, &cases, project, k, embed).map_err(|e| e.to_string())?;
        out.push_str(&format!("{name:<10} {hit:>7.3} {hit1:>7.3} {mrr:>7.3}\n"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{observations, schema};

    #[test]
    fn scores_hit_and_miss_by_topic_key() {
        let mut conn = Connection::open_in_memory().unwrap();
        schema::migrate(&mut conn).unwrap();
        for (title, content, key) in [
            (
                "build cache dir",
                "set BUILD_CACHE_DIR per task",
                "cache-dir",
            ),
            ("git worktree rule", "never edit on master", "wt-rule"),
        ] {
            observations::save(
                &conn,
                None,
                "note",
                title,
                content,
                None,
                Some("p"),
                None,
                Some(key),
            )
            .unwrap();
        }
        let cases = "{\"query\":\"BUILD_CACHE_DIR\",\"expect\":[\"cache-dir\"]}\n\
                     {\"query\":\"nonexistentzzz\",\"expect\":[\"wt-rule\"]}\n";
        let (hit, hit1, mrr) = score(
            &conn,
            &cases
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect::<Vec<Case>>(),
            Some("p"),
            3,
            |_| None,
        )
        .unwrap();
        assert_eq!((hit, hit1, mrr), (0.5, 0.5, 0.5));
        assert!(run(&conn, cases, Some("p"), 3).unwrap().contains("lexical"));
        assert!(run(&conn, cases, Some("p"), 51).is_err());
    }
}
