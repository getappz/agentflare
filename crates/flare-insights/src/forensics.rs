//! Five measured forensic detectors over stored sessions (Release 1, T9).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::analytics::cache_metrics;
use crate::capability::{self, Support};
use crate::evidence::{CoverageMeta, EvidenceRef, Finding, MetricBasis, Report};
use crate::model::Session;
use crate::store::InsightsStore;

pub const LARGE_OUTPUT_BYTES: i64 = 40_000;
const MIN_TEXT_CHARS: usize = 15;

#[derive(Debug, Clone)]
pub struct ForensicsOptions {
    pub days: u32,
    pub project_prefix: Option<String>,
    pub source: Option<String>,
    pub session_id: Option<String>,
}

impl Default for ForensicsOptions {
    fn default() -> Self {
        Self {
            days: 7,
            project_prefix: None,
            source: None,
            session_id: None,
        }
    }
}

/// Strip injected blocks, collapse whitespace, lowercase; `None` when < 15 chars.
pub fn normalise_user_text(text: &str) -> Option<String> {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    let res = RES.get_or_init(|| {
        ["SYSTEM_MESSAGE", "ADDITIONAL_METADATA", "system-reminder"]
            .iter()
            .map(|t| Regex::new(&format!("(?is)<{t}>.*?</{t}>")).unwrap())
            .collect()
    });
    let mut s = text.to_string();
    for re in res {
        s = re.replace_all(&s, " ").into_owned();
    }
    let s = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    (s.chars().count() >= MIN_TEXT_CHARS).then_some(s)
}

fn hash(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn eref(s: &Session, turn_seq: Option<u32>, tool: Option<&str>) -> EvidenceRef {
    EvidenceRef {
        source: s.source.as_str().to_string(),
        session_id: s.id.clone(),
        turn_seq,
        tool_call_id: tool.map(String::from),
        at: None,
        excerpt: None,
    }
}

fn finding(
    id: &str,
    value: Option<Value>,
    unit: &str,
    basis: MetricBasis,
    reason: Option<&str>,
    evidence: Vec<EvidenceRef>,
    note: &str,
) -> Finding {
    Finding {
        id: id.into(),
        detector: id.split('.').next().unwrap_or(id).into(),
        value,
        unit: unit.into(),
        basis,
        unavailable_reason: reason.map(String::from),
        evidence,
        note: note.into(),
    }
}

/// Run `sql_prefix + " WHERE session_id IN (...)" + sql_suffix` over `ids` in
/// chunks (SQLite bound-variable limit), concatenating mapped rows in id order.
fn query_in<T>(
    conn: &rusqlite::Connection,
    sql_prefix: &str,
    sql_suffix: &str,
    ids: &[&str],
    map: impl Fn(&rusqlite::Row) -> rusqlite::Result<T>,
) -> anyhow::Result<Vec<T>> {
    let mut out = Vec::new();
    for chunk in ids.chunks(500) {
        let marks = vec!["?"; chunk.len()].join(",");
        let mut st = conn.prepare(&format!(
            "{sql_prefix} WHERE session_id IN ({marks}) {sql_suffix}"
        ))?;
        for r in st.query_map(rusqlite::params_from_iter(chunk), &map)? {
            out.push(r?);
        }
    }
    Ok(out)
}

pub fn compute(
    store: &InsightsStore,
    opts: &ForensicsOptions,
    now: DateTime<Utc>,
) -> anyhow::Result<Report<Vec<Finding>>> {
    let since = now - Duration::days(opts.days as i64);
    let sessions: Vec<Session> = store
        .list_sessions_filtered(
            opts.source.as_deref(),
            opts.project_prefix.as_deref(),
            usize::MAX >> 1,
            0,
        )?
        .into_iter()
        .filter(|s| s.updated_at >= since)
        .filter(|s| opts.session_id.as_deref().is_none_or(|id| id == s.id))
        .collect();
    let by_id: HashMap<&str, &Session> = sessions.iter().map(|s| (s.id.as_str(), s)).collect();
    let mut ids: Vec<&str> = by_id.keys().copied().collect();
    ids.sort_unstable();
    let conn = &store.conn;

    // Turns: (session, seq, user_text, has_started_at)
    let turns: Vec<(String, u32, Option<String>, bool)> = query_in(
        conn,
        "SELECT session_id, seq, user_text, started_at IS NOT NULL FROM turns",
        "ORDER BY session_id, seq",
        &ids,
        |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u32, r.get(2)?, r.get(3)?)),
    )?;

    let with_turns: HashSet<&str> = turns.iter().map(|t| t.0.as_str()).collect();
    let timestamped: HashSet<&str> = turns.iter().filter(|t| t.3).map(|t| t.0.as_str()).collect();
    let mut cost_basis_mix: BTreeMap<String, usize> = BTreeMap::new();
    for s in sessions.iter().filter(|s| s.cost.is_some()) {
        *cost_basis_mix
            .entry(s.cost_basis.as_str().to_string())
            .or_default() += 1;
    }
    let coverage = CoverageMeta {
        window_start: since,
        window_end: now,
        sessions_in_window: sessions.len(),
        // Sessions with at least one stored turn: the transcript detectors' input.
        eligible_sessions: with_turns.len(),
        eligible_turns: turns.len(),
        unpriced_sessions: sessions.iter().filter(|s| s.cost.is_none()).count(),
        cost_basis_mix,
        // Sessions in the window with no timestamped turn (including no turns).
        sessions_missing_turn_timestamps: sessions.len() - timestamped.len(),
        failed_sources: Vec::new(),
    };

    let mut out = Vec::new();
    out.extend(repeated_user_text(&by_id, &turns));
    out.push(failed_tool_streaks(store, &sessions, &ids, &by_id)?);
    out.extend(large_output(store, &ids, &by_id)?);
    out.push(cost_concentration(&sessions));
    out.extend(cache_reuse(&sessions));

    Ok(Report {
        schema_version: 1,
        detector_version: 1,
        generated_at: now,
        filters: json!({
            "days": opts.days, "project": opts.project_prefix,
            "source": opts.source, "session": opts.session_id,
        }),
        coverage,
        capabilities: capability::capabilities().to_vec(),
        body: out,
    })
}

type Group<'a> = (String, usize, Vec<(&'a str, u32)>);

fn repeated_user_text(
    by_id: &HashMap<&str, &Session>,
    turns: &[(String, u32, Option<String>, bool)],
) -> Vec<Finding> {
    // hash -> session -> seqs, plus first raw text for the excerpt
    let mut groups: BTreeMap<String, BTreeMap<&str, Vec<u32>>> = BTreeMap::new();
    let mut sample: HashMap<String, String> = HashMap::new();
    for (sid, seq, text, _) in turns {
        let Some(norm) = text.as_deref().and_then(normalise_user_text) else {
            continue;
        };
        let h = hash(&norm);
        sample.entry(h.clone()).or_insert(norm);
        groups
            .entry(h)
            .or_default()
            .entry(sid)
            .or_default()
            .push(*seq);
    }
    let build = |within: bool| {
        let mut rows: Vec<Group> = Vec::new();
        for (h, per) in &groups {
            let total: usize = per.values().map(Vec::len).sum();
            let hit = if within {
                per.values().any(|v| v.len() >= 2)
            } else {
                per.len() >= 2
            };
            if !hit {
                continue;
            }
            let refs = per.iter().map(|(s, v)| (*s, v[0])).collect();
            rows.push((h.clone(), total, refs));
        }
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let value: Vec<Value> = rows
            .iter()
            .map(|(h, n, refs)| json!({"hash": h, "count": n, "sessions": refs.len()}))
            .collect();
        let evidence = rows
            .iter()
            .flat_map(|(h, _, refs)| {
                refs.iter()
                    .map(|(sid, seq)| eref(by_id[sid], Some(*seq), None).with_excerpt(&sample[h]))
            })
            .collect();
        (value, evidence)
    };
    [(true, "within_session"), (false, "cross_session")]
        .into_iter()
        .map(|(within, scope)| {
            let (value, evidence) = build(within);
            finding(
                &format!("repeated_user_text.{scope}"),
                Some(Value::Array(value)),
                "groups",
                MetricBasis::Measured,
                None,
                evidence,
                "normalised user text repeated; hash groups with counts",
            )
        })
        .collect()
}

fn failed_tool_streaks(
    store: &InsightsStore,
    sessions: &[Session],
    all_ids: &[&str],
    by_id: &HashMap<&str, &Session>,
) -> anyhow::Result<Finding> {
    let recorded = |s: &Session| {
        capability::for_source(s.source.as_str())
            .is_some_and(|c| c.tool_failure_status != Support::NotRecorded)
    };
    let eligible: HashSet<&str> = sessions
        .iter()
        .filter(|s| recorded(s))
        .map(|s| s.id.as_str())
        .collect();
    if eligible.is_empty() && !sessions.is_empty() {
        return Ok(finding(
            "failed_tool_streaks",
            None,
            "streaks",
            MetricBasis::Measured,
            Some("tool_failure_status_not_recorded"),
            vec![],
            "no session in the window comes from an adapter that records tool failure status",
        ));
    }
    let ids: Vec<&str> = all_ids
        .iter()
        .copied()
        .filter(|i| eligible.contains(i))
        .collect();
    let rows = query_in(
        &store.conn,
        "SELECT id, session_id, turn_seq, name, status FROM tool_calls",
        "ORDER BY session_id, turn_seq, rowid",
        &ids,
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? as u32,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        },
    )?;
    let (mut value, mut evidence) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < rows.len() {
        let mut j = i + 1;
        while j < rows.len() && rows[j].1 == rows[i].1 && rows[j].3 == rows[i].3 {
            j += 1;
        }
        if j - i >= 2 && rows[i].4 == "failed" {
            value.push(json!({
                "session_id": rows[i].1, "tool": rows[i].3,
                "length": j - i, "turn_seq": rows[i].2,
            }));
            evidence.push(eref(
                by_id[rows[i].1.as_str()],
                Some(rows[i].2),
                Some(&rows[i].0),
            ));
        }
        i = j;
    }
    let excluded = sessions.len() - eligible.len();
    Ok(finding(
        "failed_tool_streaks",
        Some(Value::Array(value)),
        "streaks",
        MetricBasis::Measured,
        None,
        evidence,
        &format!("{excluded} session(s) from adapters without tool failure status excluded"),
    ))
}

fn large_output(
    store: &InsightsStore,
    ids: &[&str],
    by_id: &HashMap<&str, &Session>,
) -> anyhow::Result<Vec<Finding>> {
    let mut rows = query_in(
        &store.conn,
        "SELECT id, session_id, turn_seq, name, length(CAST(output AS BLOB)) AS n FROM tool_calls",
        &format!("AND n >= {LARGE_OUTPUT_BYTES}"),
        ids,
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? as u32,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        },
    )?;
    rows.sort_by(|a, b| b.4.cmp(&a.4).then_with(|| a.0.cmp(&b.0)));
    let evidence: Vec<EvidenceRef> = rows
        .iter()
        .map(|r| eref(by_id[r.1.as_str()], Some(r.2), Some(&r.0)))
        .collect();
    let bytes: Vec<Value> = rows
        .iter()
        .map(|r| json!({"tool_call_id": r.0, "session_id": r.1, "tool": r.3, "bytes": r.4}))
        .collect();
    let tokens: Vec<Value> = rows
        .iter()
        .map(|r| json!({"tool_call_id": r.0, "session_id": r.1, "tool": r.3, "estimated_tokens": r.4 / 4}))
        .collect();
    Ok(vec![
        finding(
            "large_retained_tool_output",
            Some(Value::Array(bytes)),
            "bytes",
            MetricBasis::Measured,
            None,
            evidence.clone(),
            "tool outputs of at least 40000 bytes",
        ),
        finding(
            "large_retained_tool_output.estimated_tokens",
            Some(Value::Array(tokens)),
            "tokens",
            MetricBasis::Estimated {
                assumption: "4 bytes per token".into(),
            },
            None,
            evidence,
            "bytes / 4",
        ),
    ])
}

fn cost_concentration(sessions: &[Session]) -> Finding {
    let mut priced: Vec<(&Session, f64)> = sessions
        .iter()
        .filter_map(|s| s.cost.as_ref().map(|c| (s, c.total_usd)))
        .collect();
    priced.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.id.cmp(&b.0.id)));
    let total: f64 = priced.iter().map(|p| p.1).sum();
    let unpriced = sessions.len() - priced.len();
    let mut mix: BTreeMap<&str, usize> = BTreeMap::new();
    for (s, _) in &priced {
        *mix.entry(s.cost_basis.as_str()).or_default() += 1;
    }
    let top: Vec<_> = priced.iter().take(5).collect();
    let evidence = top.iter().map(|(s, _)| eref(s, None, None)).collect();
    let (value, reason) = if total > 0.0 {
        let top_usd: f64 = top.iter().map(|p| p.1).sum();
        (
            Some(json!({
                "top5_share": top_usd / total, "top5_usd": top_usd, "total_usd": total,
                "priced_sessions": priced.len(), "unpriced_sessions": unpriced,
                "cost_basis_mix": mix,
                "top": top.iter().map(|(s, c)| json!({"session_id": s.id, "cost_usd": c})).collect::<Vec<_>>(),
            })),
            None,
        )
    } else {
        (None, Some("no_priced_sessions"))
    };
    finding(
        "cost_concentration",
        value,
        "share",
        MetricBasis::Measured,
        reason,
        evidence,
        &format!("computed over priced sessions only; {unpriced} unpriced session(s) excluded"),
    )
}

fn cache_reuse(sessions: &[Session]) -> Vec<Finding> {
    let mut per: BTreeMap<&str, (u64, u64, u64)> = BTreeMap::new();
    for s in sessions {
        let e = per.entry(s.source.as_str()).or_default();
        e.0 += s.tokens.input;
        e.1 += s.tokens.cache_read;
        e.2 += s.tokens.cache_write;
    }
    per.into_iter()
        .map(|(src, (i, r, w))| {
            let recorded =
                capability::for_source(src).is_some_and(|c| c.cache_tokens == Support::Recorded);
            let (value, reason) = if !recorded {
                (None, Some("cache_not_recorded"))
            } else {
                match cache_metrics(i, r, w).0 {
                    Some(v) => (Some(json!(v)), None),
                    None => (None, Some("no_input_tokens")),
                }
            };
            finding(
                &format!("cache_reuse.{src}"),
                value,
                "share",
                MetricBasis::Measured,
                reason,
                vec![],
                "cache_read_share = read / (input + read)",
            )
        })
        .collect()
}
