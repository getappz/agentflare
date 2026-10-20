//! `search type="all"`: fan out to store + memory + code (+ web) and fuse the
//! per-arm rankings with Reciprocal Rank Fusion.
//!
//! RRF is rank-based on purpose: BM25, cosine and the opaque payloads of the
//! delegated arms (leanctx, rivalsearch) have incompatible score scales, but
//! every arm has a rank order.

use super::*;
use serde::Serialize;

/// RRF constant (the value from the original paper; also used by the store's
/// doc+chunk fusion).
const RRF_K: f64 = 60.0;

/// One entry of a single arm's ranking, best first.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ArmHit {
    pub(super) r#ref: String,
    pub(super) title: String,
    pub(super) excerpt: String,
}

/// One fused result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct SearchHit {
    pub(super) source: String,
    pub(super) r#ref: String,
    pub(super) title: String,
    pub(super) excerpt: String,
    pub(super) score: f64,
}

/// Plain RRF over `(source, ranked hits)` arms: fused score is the sum over
/// arms of `1 / (60 + rank)` with 1-based rank, no per-signal weights.
///
/// De-dup rule: the same `ref` in more than one arm is ONE hit. Its score is
/// the sum of its per-arm contributions, and it keeps the `source` (and
/// title/excerpt) of the arm that ranked it highest (earlier arm wins a tie).
/// A ref repeated inside one arm counts once, at its best rank.
/// Ties in fused score keep first-seen order.
pub(super) fn fuse_rrf(arms: Vec<(&str, Vec<ArmHit>)>, limit: usize) -> Vec<SearchHit> {
    struct Acc {
        hit: SearchHit,
        best_rank: usize,
    }
    let mut order: Vec<String> = Vec::new();
    let mut by_ref: std::collections::HashMap<String, Acc> = std::collections::HashMap::new();
    for (source, hits) in arms {
        let mut seen = std::collections::HashSet::new();
        for (idx, h) in hits.into_iter().enumerate() {
            if !seen.insert(h.r#ref.clone()) {
                continue;
            }
            let rank = idx + 1;
            let contribution = 1.0 / (RRF_K + rank as f64);
            match by_ref.get_mut(&h.r#ref) {
                Some(acc) => {
                    acc.hit.score += contribution;
                    if rank < acc.best_rank {
                        acc.best_rank = rank;
                        acc.hit.source = source.to_string();
                        acc.hit.title = h.title;
                        acc.hit.excerpt = h.excerpt;
                    }
                }
                None => {
                    order.push(h.r#ref.clone());
                    by_ref.insert(
                        h.r#ref.clone(),
                        Acc {
                            hit: SearchHit {
                                source: source.to_string(),
                                r#ref: h.r#ref,
                                title: h.title,
                                excerpt: h.excerpt,
                                score: contribution,
                            },
                            best_rank: rank,
                        },
                    );
                }
            }
        }
    }
    let mut out: Vec<SearchHit> = order
        .into_iter()
        .filter_map(|r| by_ref.remove(&r).map(|a| a.hit))
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score)); // stable: ties keep first-seen order
    out.truncate(limit);
    out
}

/// Rough token cost of a hit (~4 chars/token over what the agent receives).
fn estimate_tokens(h: &SearchHit) -> usize {
    (h.r#ref.len() + h.title.len() + h.excerpt.len())
        .div_ceil(4)
        .max(1)
}

/// Resolvable citation for a fused hit: what kind of ref it is and the ref itself.
fn citation(r: &str) -> serde_json::Value {
    let is_file_line = r.rsplit_once(':').is_some_and(|(p, n)| {
        !p.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
    });
    let kind = if r.starts_with("memory:") {
        "observation"
    } else if r.starts_with("http://") || r.starts_with("https://") {
        "url"
    } else if is_file_line {
        "file_line"
    } else {
        "doc"
    };
    serde_json::json!({ "kind": kind, "ref": r })
}

/// Greedy context-pack: walk hits best-score first, keep each one whose token
/// estimate still fits the remaining budget (a too-big hit is skipped, smaller
/// lower-ranked ones may still fill the gap). Returns `(kept, used_tokens)`;
/// `used_tokens <= budget` always.
fn pack_hits(hits: Vec<SearchHit>, budget: usize) -> (Vec<serde_json::Value>, usize) {
    let mut used = 0;
    let mut kept = Vec::new();
    for h in hits {
        let tokens = estimate_tokens(&h);
        if used + tokens > budget {
            continue;
        }
        used += tokens;
        let citation = citation(&h.r#ref);
        let mut v = serde_json::to_value(&h).unwrap_or_default();
        v["tokens"] = tokens.into();
        v["citation"] = citation;
        kept.push(v);
    }
    (kept, used)
}

fn str_field<'a>(v: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(|x| x.as_str()))
        .filter(|s| !s.is_empty())
}

/// Best-effort rank extraction from an opaque delegated-arm payload: JSON
/// arrays/`results`/MCP `content` text blocks are walked in order, objects use
/// their url/path/id as ref, and plain text yields one hit per line (a leading
/// `path:line` becomes the ref). Only the order matters to RRF.
pub(super) fn opaque_hits(val: &serde_json::Value) -> Vec<ArmHit> {
    use serde_json::Value;
    fn walk(v: &Value, out: &mut Vec<ArmHit>) {
        match v {
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::String(s) => {
                let t = s.trim();
                if t.starts_with(['[', '{'])
                    && let Ok(j) = serde_json::from_str::<Value>(t)
                {
                    return walk(&j, out);
                }
                for line in t.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    let mut parts = line.splitn(3, ':');
                    let r = match (parts.next(), parts.next()) {
                        (Some(p), Some(n))
                            if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) =>
                        {
                            format!("{p}:{n}")
                        }
                        _ => line.chars().take(200).collect(),
                    };
                    out.push(ArmHit {
                        r#ref: r,
                        title: String::new(),
                        excerpt: line.chars().take(300).collect(),
                    });
                }
            }
            Value::Object(o) => {
                if let Some(items) = o
                    .get("results")
                    .or_else(|| o.get("content"))
                    .filter(|x| x.is_array())
                {
                    for item in items.as_array().into_iter().flatten() {
                        match item.get("text") {
                            Some(t) if item.get("type").is_some_and(|ty| ty == "text") => {
                                walk(t, out)
                            }
                            _ => walk(item, out),
                        }
                    }
                } else if let Some(r) = str_field(v, &["url", "link", "path", "file", "id"]) {
                    out.push(ArmHit {
                        r#ref: r.to_string(),
                        title: str_field(v, &["title", "name"]).unwrap_or(r).to_string(),
                        excerpt: str_field(v, &["snippet", "description", "content", "text"])
                            .unwrap_or_default()
                            .chars()
                            .take(300)
                            .collect(),
                    });
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(val, &mut out);
    out
}

/// Ranked hits from a delegated arm's JSON envelope; `None` when the arm
/// reported an error (a failing arm is skipped, never fatal).
pub(super) fn delegated_arm(envelope: &str) -> Option<Vec<ArmHit>> {
    let v: serde_json::Value = serde_json::from_str(envelope).ok()?;
    if v.get("error").is_some() {
        return None;
    }
    Some(opaque_hits(v.get("results")?))
}

/// Ranked hits from the store arm's grouped envelope. Grouping by doc type
/// loses the order, so docs are put back in `search_store`'s fused/reranked
/// order via their `rank` (raw scores mix BM25 and hybrid scales); artifact
/// matches (unranked) follow.
fn store_arm(envelope: &str) -> Option<Vec<ArmHit>> {
    let v: serde_json::Value = serde_json::from_str(envelope).ok()?;
    let groups = v.get("groups")?.as_object()?;
    let mut docs: Vec<(u64, ArmHit)> = Vec::new();
    let mut artifacts: Vec<ArmHit> = Vec::new();
    for (name, entries) in groups {
        for e in entries.as_array().into_iter().flatten() {
            if name == "artifact" {
                let Some(r) = str_field(e, &["url", "id"]) else {
                    continue;
                };
                artifacts.push(ArmHit {
                    r#ref: r.to_string(),
                    title: str_field(e, &["name"]).unwrap_or(r).to_string(),
                    excerpt: str_field(e, &["snippet"]).unwrap_or_default().to_string(),
                });
            } else if let Some(r) = str_field(e, &["path", "id"]) {
                docs.push((
                    e.get("rank").and_then(|s| s.as_u64()).unwrap_or(u64::MAX),
                    ArmHit {
                        r#ref: r.to_string(),
                        title: str_field(e, &["title"]).unwrap_or(r).to_string(),
                        excerpt: str_field(e, &["snippet"]).unwrap_or_default().to_string(),
                    },
                ));
            }
        }
    }
    docs.sort_by_key(|(rank, _)| *rank); // stable
    Some(docs.into_iter().map(|(_, h)| h).chain(artifacts).collect())
}

impl AgentflareMcp {
    /// Memory observations in relevance order (the grouped `memory` arm loses it).
    fn memory_arm(q: &str, limit: usize) -> Option<Vec<ArmHit>> {
        let brain = crate::memory::store::open().ok()?;
        let obs = crate::memory::search::search(&brain, q, None, None, limit).ok()?;
        Some(
            obs.into_iter()
                .map(|o| ArmHit {
                    r#ref: format!("memory:{}", o.id),
                    title: o.title,
                    excerpt: o.content.chars().take(300).collect(),
                })
                .collect(),
        )
    }

    /// Fused `type="all"` search (`"all+web"` also queries the web arm, which
    /// is off by default). Arms that fail are skipped.
    pub(super) async fn search_all(
        &self,
        req: &SearchRequest,
        with_web: bool,
    ) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let limit = req.limit.unwrap_or(20);
        let (code, web) = tokio::join!(self.search_code(req), async {
            if with_web {
                Some(self.search_web(req).await)
            } else {
                None
            }
        });
        let store = self.search_store(req).ok();
        let code = code.ok();
        let web = web.map(|w| w.ok());
        Ok(fuse_envelopes(
            q,
            limit,
            req.budget,
            store.as_deref(),
            Self::memory_arm(q, limit),
            code.as_deref(),
            web.as_ref().map(|w| w.as_deref()),
        ))
    }
}

/// Assembles the fused response from each arm's raw output; `budget` switches to
/// a token-budgeted context pack. `None` (or an
/// unparseable / error envelope) marks the arm as skipped; `web` is `None`
/// when the web arm was not requested.
pub(super) fn fuse_envelopes(
    q: &str,
    limit: usize,
    budget: Option<usize>,
    store: Option<&str>,
    memory: Option<Vec<ArmHit>>,
    code: Option<&str>,
    web: Option<Option<&str>>,
) -> String {
    let mut arms: Vec<(&str, Vec<ArmHit>)> = Vec::new();
    let mut skipped: Vec<&str> = Vec::new();
    let mut push = |name: &'static str, hits: Option<Vec<ArmHit>>| match hits {
        Some(h) => arms.push((name, h)),
        None => skipped.push(name),
    };
    push("store", store.and_then(store_arm));
    push("memory", memory);
    push("code", code.and_then(delegated_arm));
    if let Some(web) = web {
        push("web", web.and_then(delegated_arm));
    }
    let hits = fuse_rrf(arms, limit);
    if let Some(budget) = budget {
        let found = hits.len();
        let (kept, used) = pack_hits(hits, budget);
        return serde_json::json!({
            "query": q,
            "source": "all",
            "budget": budget,
            "used_tokens": used,
            "total": kept.len(),
            "dropped": found - kept.len(),
            "hits": kept,
            "skipped": skipped,
        })
        .to_string();
    }
    serde_json::json!({
        "query": q,
        "source": "all",
        "total": hits.len(),
        "hits": hits,
        "skipped": skipped,
    })
    .to_string()
}
