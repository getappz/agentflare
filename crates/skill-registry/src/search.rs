//! BM25 search over the FTS5 index. Uses shared primitives from
//! `flare-search-kit` for query sanitization.

pub use flare_search_kit::MatchMode;
use flare_search_kit::{clamped_limit, fts_query};
use rusqlite::Connection;

#[derive(Debug, Clone, serde::Serialize)]
pub struct SkillHit {
    pub name: String,
    pub source: String,
    pub description: String,
    pub est_tokens: i64,
    pub compressed: bool,
    pub score: f64,
    pub last_used_at: i64,
    /// How to install this as an MCP server (only set for registry fallback hits).
    pub install_hint: Option<String>,
    /// Streamable HTTP URL (only set for registry hits with remotes).
    pub remote_url: Option<String>,
}

/// Exponential decay half-life in seconds (30 days).
const DECAY_HALF_LIFE: f64 = 30.0 * 86400.0;

/// Apply usage-decay penalty: skills used more recently rank slightly higher
/// than equally-relevant stale ones. `now` is seconds since epoch.
fn apply_usage_decay(hits: &mut [SkillHit], now: i64) {
    for h in hits.iter_mut() {
        if h.last_used_at == 0 {
            continue;
        }
        let elapsed = (now - h.last_used_at) as f64;
        if elapsed <= 0.0 {
            continue;
        }
        // decay = 2^(-elapsed / half_life) → 1.0 when just used, → 0.0 when ancient
        let decay = (-elapsed / DECAY_HALF_LIFE).exp2();
        // Penalty: at most 30% of the raw score, scaled by decay.
        // Newest: score * 1.0.   Oldest: score * (1.0 + 0.3).
        h.score = h.score + h.score * 0.3 * (1.0 - decay);
    }
}

pub fn search(
    conn: &Connection,
    query: &str,
    limit: usize,
    mode: MatchMode,
) -> rusqlite::Result<Vec<SkillHit>> {
    let Some(fts) = fts_query(query, mode) else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT s.name, s.source, s.description, s.est_tokens,
                s.shadow_path IS NOT NULL,
                s.last_used_at,
                bm25(skills_fts, 3.0, 1.0, 0.5, 2.0, 2.0) AS score
         FROM skills_fts
         JOIN skills s ON s.rowid = skills_fts.rowid
         WHERE skills_fts MATCH ?1
         ORDER BY score
         LIMIT ?2",
    )?;
    let mut rows: Vec<SkillHit> = stmt
        .query_map(rusqlite::params![fts, clamped_limit(limit)], |r| {
            Ok(SkillHit {
                name: r.get(0)?,
                source: r.get(1)?,
                description: r.get(2)?,
                est_tokens: r.get(3)?,
                compressed: r.get(4)?,
                last_used_at: r.get(5)?,
                score: r.get(6)?,
                install_hint: None,
                remote_url: None,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    apply_usage_decay(&mut rows, now);
    Ok(rows)
}

/// Fold registry-fallback hits into an already-fetched local result set, up
/// to `limit` total. Pure/no I/O by design -- the caller fetches `registry`
/// (typically via `gateway_registry::registry_search::search_registry`)
/// AFTER releasing whatever lock guarded the local `search()` call, so a
/// slow or hung registry request can never block other callers of the
/// local index.
pub fn merge_registry_hits(
    mut local: Vec<SkillHit>,
    limit: usize,
    registry: Vec<gateway_registry::registry_search::RegistryHit>,
) -> Vec<SkillHit> {
    let remaining = limit.saturating_sub(local.len());
    local.extend(registry.into_iter().take(remaining).map(|hit| SkillHit {
        name: hit.server,
        source: "mcp_registry".to_string(),
        description: hit.description,
        est_tokens: 0,
        compressed: false,
        score: gateway_registry::REGISTRY_FALLBACK_SCORE,
        last_used_at: 0,
        install_hint: hit.install_hint.map(|h| {
            if let Some(runtime) = h.runtime_hint {
                format!("{} {}", runtime, h.identifier)
            } else {
                format!("{}:{}", h.registry_type, h.identifier)
            }
        }),
        remote_url: hit.remote_url,
    }));
    local
}

/// Every distinct skill name currently indexed, regardless of source. Used
/// to generate `skillOverrides` entries — unlike `search`, no query/ranking
/// is needed, just the full set of names the registry knows about.
pub fn list_all_names(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT name FROM skills ORDER BY name")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{open_in_memory, rebuild};
    use crate::sources::SkillEntry;
    use std::path::PathBuf;

    fn seed() -> Connection {

        let mut conn = open_in_memory().unwrap();
        let mk = |name: &str, desc: &str, body: &str, shadow: bool| SkillEntry {
            name: name.into(),
            source: "claude-user".into(),
            path: PathBuf::from(format!("/x/{name}/SKILL.md")),
            description: desc.into(),
            body: body.into(),
            neg_text: String::new(),
            tags: String::new(),
            est_tokens: 100,
            mtime: 1,
            shadow_path: shadow.then(|| PathBuf::from(format!("/s/{name}/SKILL.md"))),
        };
        rebuild(
            &mut conn,
            &[
                mk(
                    "live",
                    "Use when the user asks about running sessions, agent status",
                    "",
                    true,
                ),
                mk(
                    "cv-usage",
                    "Use when the user asks about usage analytics, token usage, cost summary",
                    "detailed usage tracking body",
                    false,
                ),
                mk(
                    "win-cleanup",
                    "Use when the user asks to free disk space on Windows",
                    "",
                    false,
                ),
            ],
        )
        .unwrap();
        conn
    }

    fn seed_body_match() -> Connection {
        let mut conn = open_in_memory().unwrap();
        let mk = |name: &str, desc: &str, body_text: &str| SkillEntry {
            name: name.into(),
            source: "claude-user".into(),
            path: PathBuf::from(format!("/x/{name}/SKILL.md")),
            description: desc.into(),
            body: body_text.into(),
            neg_text: String::new(),
            tags: String::new(),
            est_tokens: 100,
            mtime: 1,
            shadow_path: None,
        };
        rebuild(
            &mut conn,
            &[
                mk("skill-a", "Use for general automation", "handles file parsing and data extraction"),
                mk("skill-b", "Use for general automation", "unrelated topic coverage"),
            ],
        )
        .unwrap();
        conn
    }

    #[test]
    fn all_mode_requires_every_token() {
        let conn = seed();
        let hits = search(&conn, "token usage cost", 5, MatchMode::All).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "cv-usage");
    }

    #[test]
    fn any_mode_broadens_recall() {
        let conn = seed();
        let hits = search(&conn, "sessions cost", 5, MatchMode::Any).unwrap();
        let names: Vec<_> = hits.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"live"));
        assert!(names.contains(&"cv-usage"));
    }

    #[test]
    fn name_match_outranks_description_match() {
        let conn = seed();
        // "usage" appears in cv-usage's NAME and description; "live" only names it.
        let hits = search(&conn, "usage", 5, MatchMode::Any).unwrap();
        assert_eq!(hits[0].name, "cv-usage");
    }

    #[test]
    fn compressed_flag_reflects_shadow() {
        let conn = seed();
        let hits = search(&conn, "sessions", 5, MatchMode::Any).unwrap();
        assert!(hits.iter().find(|h| h.name == "live").unwrap().compressed);
    }

    #[test]
    fn body_text_matches_outranks_identical_description_only() {
        let conn = seed_body_match();
        // "parsing" only in skill-a's body; both have same description.
        let hits = search(&conn, "parsing", 5, MatchMode::Any).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].name, "skill-a");
    }

    #[test]
    fn neg_text_match_penalizes_ranking_vs_pure_positive_hit() {
        let mut conn = open_in_memory().unwrap();
        let mk = |name: &str, desc: &str, neg: &str| SkillEntry {
            name: name.into(),
            source: "claude-user".into(),
            path: PathBuf::from(format!("/x/{name}/SKILL.md")),
            description: desc.into(),
            body: String::new(),
            neg_text: neg.into(),
            tags: String::new(),
            est_tokens: 100,
            mtime: 1,
            shadow_path: None,
        };
        rebuild(
            &mut conn,
            &[
                mk(
                    "claude-api",
                    "Use for Claude API, Anthropic access",
                    "Do not use for OpenAI GPT or other providers",
                ),
                mk("openai-tool", "Use for OpenAI GPT models", ""),
            ],
        )
        .unwrap();
        // "OpenAI" matches both: claude-api via neg_text (penalty), openai-tool via description (positive).
        // openai-tool should rank higher.
        let hits = search(&conn, "OpenAI", 5, MatchMode::Any).unwrap();
        assert_eq!(hits[0].name, "openai-tool",
            "neg_text match must penalize claude-api below the genuinely-positive hit");
    }

    #[test]
    fn fts_operators_in_query_are_neutralized() {
        let conn = seed();
        for q in [
            "cost\" OR \"x",
            "NEAR(a b)",
            "usage*",
            "(sessions)",
            "col:val",
        ] {
            // must not error; may or may not match
            search(&conn, q, 5, MatchMode::Any).unwrap();
        }
    }

    #[test]
    fn empty_query_returns_empty() {
        let conn = seed();
        assert!(search(&conn, "  ", 5, MatchMode::All).unwrap().is_empty());
    }

    #[test]
    fn search_with_a_huge_limit_does_not_panic_and_still_returns_results() {
        // usize::MAX cast straight to i64 would be -1 -- SQLite treats a
        // negative LIMIT as "no limit", silently defeating clamped_limit's cap.
        let conn = seed();
        let hits = search(&conn, "usage", usize::MAX, MatchMode::Any).unwrap();
        assert!(!hits.is_empty());
    }

    #[test]
    fn list_all_names_returns_every_skill_sorted() {
        let conn = seed();
        assert_eq!(
            list_all_names(&conn).unwrap(),
            vec![
                "cv-usage".to_string(),
                "live".to_string(),
                "win-cleanup".to_string()
            ]
        );
    }

    #[test]
    fn usage_decay_penalizes_stale_skills() {
        let now = 1_000_000_000i64;
        let recent = now - 3600;  // 1 hour ago
        let stale = now - 90 * 86400; // 90 days ago
        let mut hits = vec![
            SkillHit { name: "recent".into(), source: "s".into(), description: "d".into(), est_tokens: 100, compressed: false, score: 1.0, last_used_at: recent, install_hint: None, remote_url: None },
            SkillHit { name: "stale".into(), source: "s".into(), description: "d".into(), est_tokens: 100, compressed: false, score: 1.0, last_used_at: stale, install_hint: None, remote_url: None },
        ];
        apply_usage_decay(&mut hits, now);
        let recent_hit = hits.iter().find(|h| h.name == "recent").unwrap();
        let stale_hit = hits.iter().find(|h| h.name == "stale").unwrap();
        assert!(
            stale_hit.score > recent_hit.score,
            "stale skill should have higher (worse) score after decay: recent={} stale={}",
            recent_hit.score,
            stale_hit.score
        );
    }

    #[test]
    fn usage_decay_noop_for_never_used() {
        let mut hits = vec![SkillHit {
            name: "n".into(), source: "s".into(), description: "d".into(),
            est_tokens: 100, compressed: false, score: 0.5, last_used_at: 0,
            install_hint: None, remote_url: None,
        }];
        let original = hits[0].score;
        apply_usage_decay(&mut hits, 1_000_000);
        assert_eq!(hits[0].score, original);
    }

    #[test]
    fn merge_registry_hits_marks_fallback_hits_as_registry_sourced() {
        let registry = vec![gateway_registry::registry_search::RegistryHit {
            server: "some-server".to_string(),
            description: "from the registry".to_string(),
            install_hint: None,
            remote_url: None,
        }];
        let merged = merge_registry_hits(vec![], 1, registry);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].source, "mcp_registry",
            "registry-fallback hits must carry a distinguishable source, not an empty string local skills could also have"
        );
    }
}
