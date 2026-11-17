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

#[derive(Deserialize)]
struct Resp {
    #[serde(default)]
    skills: Vec<Skill>,
}

#[derive(Deserialize)]
struct Skill {
    id: String,
    source: String,
    #[serde(rename = "skillId")]
    skill_id: String,
    #[serde(default)]
    installs: u64,
}

fn hit(s: Skill) -> SkillHit {
    SkillHit {
        // Our installs stage into ~/.agentflare/skills (not per-agent dirs),
        // resolving a plain skill name against the configured registries.
        install_hint: Some(format!(
            "agentflare skill registry add gh:{} && agentflare skill install {}",
            s.source, s.skill_id
        )),
        remote_url: Some(format!("https://skills.sh/{}", s.id)),
        description: format!("{} · {} installs", s.source, s.installs),
        name: s.skill_id,
        source: "skills.sh".to_string(),
        est_tokens: 0,
        compressed: false,
        score: gateway_registry::REGISTRY_FALLBACK_SCORE,
        confidence: 0.0,
        last_used_at: 0,
        bandit_alpha: 1.0,
        bandit_beta: 1.0,
    }
}

/// Up to `limit` skills.sh results for `query`, in the directory's own ranking.
pub fn search_skills_sh(query: &str, limit: usize) -> Vec<SkillHit> {
    let query = query.trim();
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
        .map(|r| r.skills.into_iter().take(limit).map(hit).collect())
        .unwrap_or_default()
}

/// Local confidence at which a hit is trusted enough to skip the network.
const STRONG_CONFIDENCE: f64 = 0.8;
/// Slots external hits may take from weak local matches.
const RESERVED: usize = 2;

/// External sources are worth a request when local results are short or
/// all weak (e.g. a one-word overlap on a multi-word query).
pub fn needs_external(local: &[SkillHit], limit: usize) -> bool {
    local.len() < limit || !local.iter().any(|h| h.confidence >= STRONG_CONFIDENCE)
}

/// Append `external` hits, evicting the weakest (last) local hits if needed
/// so at least `RESERVED` external hits fit within `limit`.
pub fn merge_external_hits(
    mut local: Vec<SkillHit>,
    limit: usize,
    external: Vec<SkillHit>,
) -> Vec<SkillHit> {
    local.truncate(limit.saturating_sub(external.len().min(RESERVED)));
    let room = limit.saturating_sub(local.len());
    local.extend(external.into_iter().take(room));
    local
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(id: &str) -> Skill {
        Skill {
            id: format!("o/r/{id}"),
            source: "o/r".to_string(),
            skill_id: id.to_string(),
            installs: 42,
        }
    }

    fn h(name: &str, confidence: f64) -> SkillHit {
        SkillHit {
            confidence,
            ..hit(skill(name))
        }
    }

    #[test]
    fn hit_maps_directory_entry() {
        let h = hit(skill("egm-draft"));
        assert_eq!(h.name, "egm-draft");
        assert_eq!(h.source, "skills.sh");
        assert_eq!(h.description, "o/r · 42 installs");
        assert_eq!(
            h.install_hint.as_deref(),
            Some("agentflare skill registry add gh:o/r && agentflare skill install egm-draft")
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
        assert_eq!(r.skills[0].skill_id, "c");
        assert_eq!(r.skills[0].installs, 7);
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
                .all(|h| h.description.starts_with("google/elephant-goldfish-model")),
            "got: {:?}",
            hits.iter().map(|h| &h.description).collect::<Vec<_>>()
        );
        assert_eq!(hits.len(), 5);
    }

    #[test]
    fn merge_reserves_slots_for_external_over_weak_local() {
        let local = (0..5).map(|i| h(&format!("l{i}"), 0.6)).collect();
        let ext = (0..4).map(|i| h(&format!("g{i}"), 0.0)).collect();
        let names: Vec<_> = merge_external_hits(local, 5, ext)
            .into_iter()
            .map(|x| x.name)
            .collect();
        assert_eq!(names, ["l0", "l1", "l2", "g0", "g1"]);
    }

    #[test]
    fn external_skipped_only_when_full_and_strong() {
        let strong: Vec<_> = (0..3).map(|i| h(&format!("a{i}"), 0.9)).collect();
        assert!(!needs_external(&strong, 3));
        assert!(needs_external(&strong, 5));
        let weak: Vec<_> = (0..3).map(|i| h(&format!("a{i}"), 0.6)).collect();
        assert!(needs_external(&weak, 3));
    }
}
