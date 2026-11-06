//! Recurring-request discovery: cluster similar user prompts into printed proposals.
//! Never writes files (Release 2, T11).

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::capability;
use crate::evidence::{CoverageMeta, EvidenceRef, Report};
use crate::forensics::normalise_user_text;
use crate::model::Session;
use crate::store::InsightsStore;

/// Default Jaccard threshold: maximises F1 on `tests/fixtures/discover/labelled.json`.
/// Update whenever the fixture or normalisation changes; the fixture test prints the sweep.
pub const DEFAULT_JACCARD_THRESHOLD: f64 = 0.60;

const FENCE_DROP_CHARS: usize = 500;
const NGRAM_MIN: usize = 3;
const NGRAM_MAX: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Destination {
    Rule,
    Skill,
    Command,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub cluster_id: String,
    pub destination: Destination,
    pub distinct_sessions: usize,
    pub eligible_sessions: usize,
    pub occurrences: usize,
    pub examples: Vec<EvidenceRef>,
    pub draft: String,
    pub rationale: String,
}

#[derive(Debug, Clone)]
pub struct DiscoverOptions {
    pub days: u32,
    pub project_prefix: Option<String>,
    pub min_sessions: usize,
    pub min_count: usize,
    pub jaccard_threshold: f64,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        Self {
            days: 30,
            project_prefix: None,
            min_sessions: 2,
            min_count: 3,
            jaccard_threshold: DEFAULT_JACCARD_THRESHOLD,
        }
    }
}

#[derive(Clone)]
struct Doc {
    session_id: String,
    turn_seq: u32,
    source: String,
    text: String,
    tokens: Vec<String>,
    token_set: HashSet<String>,
}

/// Drop fenced code blocks whose body exceeds 500 chars, then apply Task 9 normalisation.
pub fn normalise_discover_text(text: &str) -> Option<String> {
    normalise_user_text(&strip_large_fences(text))
}

fn strip_large_fences(text: &str) -> String {
    static FENCE: OnceLock<Regex> = OnceLock::new();
    let re = FENCE.get_or_init(|| Regex::new(r"(?s)```[^\n]*\n.*?```").unwrap());
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for m in re.find_iter(text) {
        out.push_str(&text[last..m.start()]);
        let body = m.as_str();
        // Keep short fences; drop large ones (replace with space so tokens stay separated).
        if body.chars().count() > FENCE_DROP_CHARS {
            out.push(' ');
        } else {
            out.push_str(body);
        }
        last = m.end();
    }
    out.push_str(&text[last..]);
    out
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

fn ngrams(tokens: &[String]) -> impl Iterator<Item = String> + '_ {
    (NGRAM_MIN..=NGRAM_MAX).flat_map(move |n| {
        tokens
            .windows(n)
            .map(|w| w.join(" "))
            .collect::<Vec<_>>()
            .into_iter()
    })
}

struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }
    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let mut ra = self.find(a);
        let mut rb = self.find(b);
        if ra == rb {
            return;
        }
        if self.rank[ra] < self.rank[rb] {
            std::mem::swap(&mut ra, &mut rb);
        }
        self.parent[rb] = ra;
        if self.rank[ra] == self.rank[rb] {
            self.rank[ra] += 1;
        }
    }
}

fn leading_rule_phrase(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("always ")
        || t.starts_with("never ")
        || t.starts_with("do not ")
        || t.starts_with("don't ")
        || t.starts_with("dont ")
        || t.starts_with("must ")
}

fn trailing_varies_as_id(tok: &str) -> bool {
    if tok.parse::<i64>().is_ok() {
        return true;
    }
    if tok.contains('/') || tok.starts_with('.') {
        return true;
    }
    // staging-1, env_42, id-abc style
    let has_digit = tok.chars().any(|c| c.is_ascii_digit());
    let has_sep = tok.contains('-') || tok.contains('_');
    has_digit && (has_sep || tok.chars().all(|c| c.is_ascii_alphanumeric()))
}

fn classify_destination(texts: &[&str]) -> (Destination, String) {
    if texts.iter().any(|t| leading_rule_phrase(t)) {
        return (
            Destination::Rule,
            "leading always|never|do not|don't|must verb phrase".into(),
        );
    }

    // Command: ≥70% share first two tokens; trailing token varies as number/id/path.
    let mut prefix_counts: HashMap<(&str, &str), usize> = HashMap::new();
    let mut parsed: Vec<Option<(&str, &str, &str)>> = Vec::with_capacity(texts.len());
    for t in texts {
        let parts: Vec<&str> = t.split_whitespace().collect();
        if parts.len() >= 3 {
            let key = (parts[0], parts[1]);
            *prefix_counts.entry(key).or_default() += 1;
            parsed.push(Some((parts[0], parts[1], parts[parts.len() - 1])));
        } else {
            parsed.push(None);
        }
    }
    if let Some((&(a, b), &count)) = prefix_counts.iter().max_by_key(|(_, c)| *c) {
        let share = count as f64 / texts.len() as f64;
        if share >= 0.70 {
            let trailings: Vec<&str> = parsed
                .iter()
                .flatten()
                .filter(|(x, y, _)| *x == a && *y == b)
                .map(|(_, _, t)| *t)
                .collect();
            let unique: HashSet<&str> = trailings.iter().copied().collect();
            if unique.len() >= 2 && trailings.iter().all(|t| trailing_varies_as_id(t)) {
                return (
                    Destination::Command,
                    format!(
                        "≥70% share first two tokens `{a} {b}`; trailing token varies as id/path/number"
                    ),
                );
            }
        }
    }

    (
        Destination::None,
        "no rule/command/skill pattern matched".into(),
    )
}

fn classify(docs: &[&Doc], distinct_sessions: usize) -> (Destination, String) {
    let texts: Vec<&str> = docs.iter().map(|d| d.text.as_str()).collect();
    let (dest, why) = classify_destination(&texts);
    if !matches!(dest, Destination::None) {
        return (dest, why);
    }
    let avg_tokens = if docs.is_empty() {
        0.0
    } else {
        docs.iter().map(|d| d.tokens.len()).sum::<usize>() as f64 / docs.len() as f64
    };
    if distinct_sessions >= 3 && avg_tokens >= 12.0 {
        return (
            Destination::Skill,
            format!("≥3 sessions ({distinct_sessions}) and avg tokens {avg_tokens:.1} ≥ 12"),
        );
    }
    (
        Destination::None,
        "ambiguous: no leading rule phrase, no command template, skill thresholds unmet".into(),
    )
}

/// (session id, seq, user text, has a valid timestamp)
type TurnRow = (String, u32, Option<String>, bool);

fn query_turns(store: &InsightsStore, ids: &[&str]) -> anyhow::Result<Vec<TurnRow>> {
    let conn = &store.conn;
    let mut out = Vec::new();
    for chunk in ids.chunks(500) {
        let marks = vec!["?"; chunk.len()].join(",");
        let mut st = conn.prepare(&format!(
            "SELECT session_id, seq, user_text, julianday(started_at) IS NOT NULL FROM turns WHERE session_id IN ({marks}) ORDER BY session_id, seq"
        ))?;
        for r in st.query_map(rusqlite::params_from_iter(chunk), |row| {
            Ok((
                row.get(0)?,
                row.get::<_, i64>(1)? as u32,
                row.get(2)?,
                row.get(3)?,
            ))
        })? {
            out.push(r?);
        }
    }
    Ok(out)
}

/// Mine recurring user requests into ranked proposals. Read-only; never writes files.
pub fn mine(
    store: &InsightsStore,
    opts: &DiscoverOptions,
    now: DateTime<Utc>,
) -> anyhow::Result<Report<Vec<Proposal>>> {
    let since = now.checked_sub_signed(Duration::days(opts.days as i64));
    let sessions = store.sessions_in_window(since, None, opts.project_prefix.as_deref(), None)?;
    let by_id: HashMap<&str, &Session> = sessions.iter().map(|s| (s.id.as_str(), s)).collect();
    let mut ids: Vec<&str> = by_id.keys().copied().collect();
    ids.sort_unstable();

    let turns = query_turns(store, &ids)?;
    let with_turns: HashSet<&str> = turns.iter().map(|t| t.0.as_str()).collect();
    let untimed: HashSet<&str> = turns
        .iter()
        .filter(|t| !t.3)
        .map(|t| t.0.as_str())
        .collect();

    let mut docs: Vec<Doc> = Vec::new();
    for (sid, seq, user_text, _) in &turns {
        let Some(raw) = user_text.as_deref() else {
            continue;
        };
        let Some(text) = normalise_discover_text(raw) else {
            continue;
        };
        let tokens: Vec<String> = text.split_whitespace().map(str::to_string).collect();
        if tokens.len() < NGRAM_MIN {
            continue;
        }
        let token_set = tokens.iter().cloned().collect();
        let source = by_id
            .get(sid.as_str())
            .map(|s| s.source.as_str().to_string())
            .unwrap_or_else(|| "unknown".into());
        docs.push(Doc {
            session_id: sid.clone(),
            turn_seq: *seq,
            source,
            text,
            tokens,
            token_set,
        });
    }

    // Inverted index: n-gram → session ids (and doc indices).
    let mut ngram_sessions: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut ngram_docs: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, doc) in docs.iter().enumerate() {
        let mut seen = HashSet::new();
        for ng in ngrams(&doc.tokens) {
            if seen.insert(ng.clone()) {
                ngram_sessions
                    .entry(ng.clone())
                    .or_default()
                    .insert(doc.session_id.clone());
                ngram_docs.entry(ng).or_default().push(i);
            }
        }
    }

    // Connected components never depend on pair order, so nothing here stores pairs.
    // Identical prompts match by definition: collapse them to one representative so a
    // prompt repeated thousands of times costs one comparison, not a quadratic number.
    let mut uf = UnionFind::new(docs.len());
    let mut representative: HashMap<Vec<&str>, usize> = HashMap::new();
    let mut is_representative = vec![false; docs.len()];
    for (i, doc) in docs.iter().enumerate() {
        let mut key: Vec<&str> = doc.token_set.iter().map(String::as_str).collect();
        key.sort_unstable();
        match representative.entry(key) {
            Entry::Occupied(first) => {
                if opts.jaccard_threshold <= 1.0 {
                    uf.union(i, *first.get());
                } else {
                    is_representative[i] = true;
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(i);
                is_representative[i] = true;
            }
        }
    }

    // Candidates share an n-gram present in >= min_sessions sessions. Skip pairs already
    // connected, and pairs whose sizes alone rule out the threshold (J <= min/max).
    for (ng, sessions_hit) in &ngram_sessions {
        if sessions_hit.len() < opts.min_sessions {
            continue;
        }
        let idxs: Vec<usize> = ngram_docs
            .get(ng)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&i| is_representative[i])
            .collect();
        for (n, &i) in idxs.iter().enumerate() {
            let size_i = docs[i].token_set.len();
            for &j in &idxs[n + 1..] {
                let size_j = docs[j].token_set.len();
                if (size_i.min(size_j) as f64) < opts.jaccard_threshold * size_i.max(size_j) as f64
                    || uf.find(i) == uf.find(j)
                {
                    continue;
                }
                if jaccard(&docs[i].token_set, &docs[j].token_set) >= opts.jaccard_threshold {
                    uf.union(i, j);
                }
            }
        }
    }

    let mut clusters: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..docs.len() {
        let root = uf.find(i);
        clusters.entry(root).or_default().push(i);
    }

    let eligible_sessions = docs
        .iter()
        .map(|d| d.session_id.as_str())
        .collect::<HashSet<_>>()
        .len();

    let mut proposals: Vec<Proposal> = Vec::new();
    for mut members in clusters.into_values() {
        members.sort_unstable();
        let occurrences = members.len();
        let distinct: BTreeSet<&str> = members
            .iter()
            .map(|&i| docs[i].session_id.as_str())
            .collect();
        if occurrences < opts.min_count || distinct.len() < opts.min_sessions {
            continue;
        }
        let member_docs: Vec<&Doc> = members.iter().map(|&i| &docs[i]).collect();
        let (destination, rationale) = classify(&member_docs, distinct.len());

        // Stable draft: lexicographically first normalised text.
        let mut texts: Vec<&str> = member_docs.iter().map(|d| d.text.as_str()).collect();
        texts.sort_unstable();
        let draft = texts.first().copied().unwrap_or("").to_string();

        let mut id_material = texts.join("\n");
        id_material.push('\n');
        for sid in &distinct {
            id_material.push_str(sid);
            id_material.push('\n');
        }
        let cluster_id = hex::encode(Sha256::digest(id_material.as_bytes()));

        let mut examples: Vec<EvidenceRef> = members
            .iter()
            .map(|&i| {
                let d = &docs[i];
                EvidenceRef {
                    source: d.source.clone(),
                    session_id: d.session_id.clone(),
                    turn_seq: Some(d.turn_seq),
                    tool_call_id: None,
                    at: None,
                    excerpt: None,
                }
                .with_excerpt(&d.text)
            })
            .collect();
        // Stable order: session_id, turn_seq
        examples.sort_by(|a, b| (&a.session_id, a.turn_seq).cmp(&(&b.session_id, b.turn_seq)));

        proposals.push(Proposal {
            cluster_id,
            destination,
            distinct_sessions: distinct.len(),
            eligible_sessions,
            occurrences,
            examples,
            draft,
            rationale,
        });
    }

    proposals.sort_by(|a, b| {
        b.distinct_sessions
            .cmp(&a.distinct_sessions)
            .then(b.occurrences.cmp(&a.occurrences))
            .then(a.cluster_id.cmp(&b.cluster_id))
    });

    let coverage = CoverageMeta {
        window_start: since.unwrap_or(DateTime::<Utc>::MIN_UTC),
        window_end: now,
        sessions_in_window: sessions.len(),
        eligible_sessions,
        eligible_turns: docs.len(),
        unpriced_sessions: sessions.iter().filter(|s| s.cost.is_none()).count(),
        cost_basis_mix: BTreeMap::new(),
        // Same meaning as `stats_window`: no turns, or any turn without a valid timestamp.
        sessions_missing_turn_timestamps: sessions
            .iter()
            .filter(|s| !with_turns.contains(s.id.as_str()) || untimed.contains(s.id.as_str()))
            .count(),
        failed_sources: Vec::new(),
    };

    Ok(Report {
        schema_version: 1,
        detector_version: 1,
        generated_at: now,
        filters: json!({
            "days": opts.days,
            "project": opts.project_prefix,
            "min_sessions": opts.min_sessions,
            "min_count": opts.min_count,
            "jaccard_threshold": opts.jaccard_threshold,
        }),
        coverage,
        capabilities: capability::capabilities().to_vec(),
        body: proposals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_fence_is_dropped_before_normalise() {
        let big = format!(
            "please review this ```\n{}\n``` carefully now thanks",
            "x".repeat(600)
        );
        let n = normalise_discover_text(&big).unwrap();
        assert!(!n.contains('x'));
        assert!(n.contains("please review this"));
    }

    #[test]
    fn short_fence_is_kept() {
        let t = "please review this ```\nshort\n``` carefully now thanks";
        let n = normalise_discover_text(t).unwrap();
        assert!(n.contains("short"));
    }
}
