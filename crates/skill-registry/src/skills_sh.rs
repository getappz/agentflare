//! skills.sh (Vercel's skills directory, what `npx skills find` queries)
//! fallback for `skill_search`. Unauthenticated, ranked by installs.
//! Best-effort -- any failure yields no hits.
//!
//! An earlier GitHub Code Search source lives in git history (5d138ec9):
//! it needs a token, is rate-limited, and ranks by file content.

use crate::search::SkillHit;
use serde::Deserialize;
use std::time::Duration;

const API: &str = "https://skills.sh/api/search";
const TIMEOUT: Duration = Duration::from_secs(4);
/// A caller-supplied limit must not turn into an unbounded request against a
/// public endpoint (the local search path clamps separately).
pub const MAX_REMOTE_LIMIT: usize = 100;

#[derive(Deserialize)]
struct Resp {
    #[serde(default)]
    skills: Vec<Skill>,
}

#[derive(Deserialize)]
struct Skill {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(rename = "skillId", default)]
    skill_id: Option<String>,
    #[serde(default)]
    installs: u64,
}

/// `source`, `skillId` and `id` are interpolated into an install command and a
/// URL that the MCP client may act on. Accept only the characters those fields
/// legitimately use; anything else is dropped rather than passed through.
fn safe_component(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn valid_source(source: &str) -> bool {
    let mut parts = source.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(owner), Some(repo), None) if safe_component(owner) && safe_component(repo)
    )
}

fn valid_id(id: &str) -> bool {
    let parts: Vec<_> = id.split('/').collect();
    parts.len() == 3 && parts.iter().all(|part| safe_component(part))
}

fn hit(s: Skill) -> Option<SkillHit> {
    let (Some(id), Some(source), Some(skill_id)) = (s.id, s.source, s.skill_id) else {
        return None;
    };
    if !valid_id(&id) || !valid_source(&source) || !safe_component(&skill_id) {
        return None;
    }
    // Our installs stage into ~/.agentflare/skills (not per-agent dirs),
    // resolving a plain skill name against the configured registries.
    let install_hint = format!("agentflare skill install github:{source}@{skill_id}");
    Some(SkillHit::external(
        skill_id.clone(),
        "skills.sh",
        format!("{source} · {} installs", s.installs),
        Some(install_hint),
        Some(format!("https://skills.sh/{id}")),
    ))
}

/// Up to `limit` skills.sh results for `query`, in the directory's own ranking.
pub fn search_skills_sh(query: &str, limit: usize) -> Vec<SkillHit> {
    search(query, limit).into_iter().filter_map(hit).collect()
}

/// Resolve an exact skill ID to its repository, following directory ranking.
pub fn exact_github_source(name: &str) -> Option<String> {
    search(name, 10).into_iter().find_map(|skill| {
        let source = skill.source.as_deref()?;
        (skill.skill_id.as_deref() == Some(name) && valid_source(source))
            .then(|| format!("github:{source}"))
    })
}

fn search(query: &str, limit: usize) -> Vec<Skill> {
    let query = query.trim();
    let limit = limit.min(MAX_REMOTE_LIMIT);
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }
    ureq::get(API)
        .query("q", query)
        .query("limit", &limit.to_string())
        .timeout(TIMEOUT)
        .call()
        .ok()
        .and_then(|r| r.into_json::<Resp>().ok())
        .map(|r| r.skills.into_iter().take(limit).collect())
        .unwrap_or_default()
}

/// An external request is worthwhile only when local search left open slots.
/// Local hits have already passed `is_confident()`; evicting them for
/// confidence-less directory entries would demote real matches.
pub fn needs_external(local: &[SkillHit], limit: usize) -> bool {
    local.len() < limit
}

/// Append `external` hits into the slots left open by `local`, never evicting
/// a local match.
pub fn merge_external_hits(
    mut local: Vec<SkillHit>,
    limit: usize,
    external: Vec<SkillHit>,
) -> Vec<SkillHit> {
    let room = limit.saturating_sub(local.len());
    local.extend(external.into_iter().take(room));
    local
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(id: &str) -> Skill {
        Skill {
            id: Some(format!("o/r/{id}")),
            source: Some("o/r".to_string()),
            skill_id: Some(id.to_string()),
            installs: 42,
        }
    }

    fn h(name: &str, confidence: f64) -> SkillHit {
        SkillHit {
            confidence,
            ..hit(skill(name)).unwrap()
        }
    }

    #[test]
    fn hit_maps_directory_entry() {
        let h = hit(skill("egm-draft")).unwrap();
        assert_eq!(h.name, "egm-draft");
        assert_eq!(h.source, "skills.sh");
        assert_eq!(h.description, "o/r · 42 installs");
        assert_eq!(
            h.install_hint.as_deref(),
            Some("agentflare skill install github:o/r@egm-draft")
        );
        assert_eq!(
            h.remote_url.as_deref(),
            Some("https://skills.sh/o/r/egm-draft")
        );
    }

    #[test]
    fn parses_api_response_ignoring_extra_fields() {
        let json = r#"{"query":"q","skills":[{"id":"a/b/c","source":"a/b","skillId":"c","name":"c","installs":7}],"count":1}"#;
        let r: Resp = serde_json::from_str(json).unwrap();
        assert_eq!(r.skills[0].skill_id.as_deref(), Some("c"));
        assert_eq!(r.skills[0].installs, 7);
    }

    #[test]
    fn malformed_and_null_fields_are_dropped_without_killing_the_response() {
        let json = r#"{"skills":[
            {"id":"a/b/good","source":"a/b","skillId":"good","installs":1},
            {"id":"a/b/bad","source":"a/b; rm -rf ~","skillId":"bad","installs":2},
            {"id":"a/b/evil","source":"a/b","skillId":"x && whoami","installs":3},
            {"id":null,"source":"a/b","skillId":"null-id","installs":4},
            {"id":"a/b/missing-source","skillId":"missing-source","installs":5}
        ]}"#;
        let r: Resp = serde_json::from_str(json).unwrap();
        let hits: Vec<_> = r.skills.into_iter().filter_map(hit).collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "good");
    }

    #[test]
    fn blank_query_makes_no_request() {
        assert!(search_skills_sh("  ", 5).is_empty());
        assert!(search_skills_sh("x", 0).is_empty());
    }

    /// Needs network: `cargo test -p <pkg> -- --ignored live_`.
    #[test]
    #[ignore]
    fn live_finds_google_elephant_goldfish_skills() {
        let hits = search_skills_sh("elephant goldfish model", 5);
        assert!(
            hits.iter()
                .any(|h| h.description.starts_with("google/elephant-goldfish-model")),
            "got: {:?}",
            hits.iter().map(|h| &h.description).collect::<Vec<_>>()
        );
        assert!(!hits.is_empty());
    }

    #[test]
    fn merge_never_displaces_local_hits() {
        let local = (0..5).map(|i| h(&format!("l{i}"), 0.6)).collect();
        let ext = (0..4).map(|i| h(&format!("g{i}"), 0.0)).collect();
        let names: Vec<_> = merge_external_hits(local, 5, ext)
            .into_iter()
            .map(|x| x.name)
            .collect();
        assert_eq!(names, ["l0", "l1", "l2", "l3", "l4"]);
    }

    #[test]
    fn merge_fills_only_open_slots() {
        let local = (0..2).map(|i| h(&format!("l{i}"), 0.6)).collect();
        let ext = (0..4).map(|i| h(&format!("g{i}"), 0.0)).collect();
        let names: Vec<_> = merge_external_hits(local, 5, ext)
            .into_iter()
            .map(|x| x.name)
            .collect();
        assert_eq!(names, ["l0", "l1", "g0", "g1", "g2"]);
    }

    #[test]
    fn merge_with_no_external_preserves_local() {
        let local = (0..3).map(|i| h(&format!("l{i}"), 0.6)).collect();
        let names: Vec<_> = merge_external_hits(local, 5, vec![])
            .into_iter()
            .map(|x| x.name)
            .collect();
        assert_eq!(names, ["l0", "l1", "l2"]);
    }

    #[test]
    fn external_skipped_only_when_local_fills_limit() {
        let full: Vec<_> = (0..3).map(|i| h(&format!("a{i}"), 0.6)).collect();
        assert!(!needs_external(&full, 3));
        assert!(needs_external(&full, 5));
        assert!(!needs_external(&[], 0));
    }
}
