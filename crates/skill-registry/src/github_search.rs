//! GitHub Code Search fallback for `skill_search`: finds `SKILL.md` files in
//! public repos whose content matches the query (same approach as
//! `gh skill search`). Best-effort -- any failure yields no hits.

use crate::frontmatter::parse_frontmatter;
use crate::search::SkillHit;
use serde::Deserialize;
use std::time::Duration;

const API: &str = "https://api.github.com/search/code";
const TIMEOUT: Duration = Duration::from_secs(4);
/// Code search is rate-limited (~10/min) and each hit costs one raw fetch.
const MAX_HITS: usize = 8;
/// Candidates pulled from code search (one request) before re-ranking by stars.
const SEARCH_PAGE: usize = 30;

#[derive(Deserialize)]
struct Resp {
    #[serde(default)]
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    path: String,
    repository: Repo,
}

#[derive(Deserialize)]
struct Repo {
    full_name: String,
}

/// `GITHUB_TOKEN` / `GH_TOKEN`, else whatever `gh auth token` prints.
/// Code search rejects anonymous requests, so no token means no hits.
fn token() -> Option<String> {
    let env = ["GITHUB_TOKEN", "GH_TOKEN"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()));
    env.or_else(|| {
        let out = std::process::Command::new("gh")
            .args(["auth", "token"])
            .output()
            .ok()?;
        let t = String::from_utf8(out.stdout).ok()?.trim().to_string();
        (out.status.success() && !t.is_empty()).then_some(t)
    })
}

/// Words stripped of search-qualifier syntax, so user input can't inject
/// `repo:`/`org:` filters; `None` when nothing searchable is left.
fn words(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

fn skill_name(path: &str, fm_name: Option<String>) -> String {
    fm_name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
        let mut parts = path.rsplit('/');
        parts.next(); // SKILL.md
        parts.next().unwrap_or("skill").to_string()
    })
}

fn hit(repo: &str, path: &str, text: Option<&str>) -> SkillHit {
    let fm = text.and_then(parse_frontmatter).map(|(fm, _)| fm);
    let description = fm
        .as_ref()
        .and_then(|f| f.description.clone())
        .unwrap_or_default();
    let name = skill_name(path, fm.and_then(|f| f.name));
    SkillHit {
        install_hint: Some(format!("gh skill install {repo} {name}")),
        remote_url: Some(format!("https://github.com/{repo}/blob/HEAD/{path}")),
        name,
        source: "github".to_string(),
        description,
        est_tokens: 0,
        compressed: false,
        score: gateway_registry::REGISTRY_FALLBACK_SCORE,
        confidence: 0.0,
        last_used_at: 0,
        bandit_alpha: 1.0,
        bandit_beta: 1.0,
    }
}

fn fetch_skill_md(repo: &str, path: &str) -> Option<String> {
    ureq::get(&format!(
        "https://raw.githubusercontent.com/{repo}/HEAD/{path}"
    ))
    .timeout(TIMEOUT)
    .call()
    .ok()?
    .into_string()
    .ok()
}

fn repo_stars(repo: &str, token: &str) -> u64 {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        stargazers_count: u64,
    }
    ureq::get(&format!("https://api.github.com/repos/{repo}"))
        .set("Authorization", &format!("Bearer {token}"))
        .timeout(TIMEOUT)
        .call()
        .ok()
        .and_then(|r| r.into_json::<R>().ok())
        .map_or(0, |r| r.stargazers_count)
}

fn code_search(q: &str, per_page: usize, token: &str) -> Vec<Item> {
    ureq::get(API)
        .query("q", q)
        .query("per_page", &per_page.to_string())
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/vnd.github+json")
        .timeout(TIMEOUT)
        .call()
        .ok()
        .and_then(|r| r.into_json::<Resp>().ok())
        .map(|r| r.items)
        .unwrap_or_default()
}

/// Up to `limit` GitHub skills matching all words of `query`, most-starred repos first.
pub fn search_github(query: &str, limit: usize) -> Vec<SkillHit> {
    let words = words(query);
    if words.is_empty() || limit == 0 {
        return Vec::new();
    }
    let Some(token) = token() else {
        return Vec::new();
    };
    // Code search ranks by content match, which buries a famous repo under
    // copies of it; re-rank by repo stars, like `gh skill search` reports.
    let q = format!("{} filename:SKILL.md", words.join(" "));
    let mut seen = std::collections::HashSet::new();
    let mut items: Vec<Item> = code_search(&q, SEARCH_PAGE, &token)
        .into_iter()
        .filter(|i| seen.insert((i.repository.full_name.clone(), i.path.clone())))
        .collect();
    let mut repos: Vec<&str> = items
        .iter()
        .map(|i| i.repository.full_name.as_str())
        .collect();
    repos.sort_unstable();
    repos.dedup();
    let stars: std::collections::HashMap<String, u64> = std::thread::scope(|s| {
        let handles: Vec<_> = repos
            .iter()
            .map(|r| (r.to_string(), s.spawn(|| repo_stars(r, &token))))
            .collect();
        handles
            .into_iter()
            .map(|(r, h)| (r, h.join().unwrap_or(0)))
            .collect()
    });
    items.sort_by_key(|i| std::cmp::Reverse(stars[&i.repository.full_name])); // stable
    items.truncate(limit.min(MAX_HITS));
    std::thread::scope(|s| {
        let handles: Vec<_> = items
            .iter()
            .map(|i| {
                s.spawn(|| {
                    let text = fetch_skill_md(&i.repository.full_name, &i.path);
                    hit(&i.repository.full_name, &i.path, text.as_deref())
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
}

/// Local confidence at which a hit is trusted enough to skip the network.
const STRONG_CONFIDENCE: f64 = 0.8;
/// Slots GitHub hits may take from weak local matches.
const RESERVED: usize = 2;

/// External sources are worth a request when local results are short or
/// all weak (e.g. a one-word overlap on a multi-word query).
pub fn needs_external(local: &[SkillHit], limit: usize) -> bool {
    local.len() < limit || !local.iter().any(|h| h.confidence >= STRONG_CONFIDENCE)
}

/// Append `github` hits, evicting the weakest (last) local hits if needed so
/// at least `RESERVED` GitHub hits fit within `limit`.
pub fn merge_github_hits(
    mut local: Vec<SkillHit>,
    limit: usize,
    github: Vec<SkillHit>,
) -> Vec<SkillHit> {
    local.truncate(limit.saturating_sub(github.len().min(RESERVED)));
    let room = limit.saturating_sub(local.len());
    local.extend(github.into_iter().take(room));
    local
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(name: &str, confidence: f64) -> SkillHit {
        SkillHit {
            confidence,
            ..hit("o/r", &format!("skills/{name}/SKILL.md"), None)
        }
    }

    /// Needs network + a GitHub token: `cargo test -p <pkg> -- --ignored live_`.
    #[test]
    #[ignore]
    fn live_finds_google_elephant_goldfish_skills() {
        let hits = search_github("elephant goldfish model", 5);
        assert!(
            hits.iter().any(|h| h
                .install_hint
                .as_deref()
                .is_some_and(|i| i.contains("google/elephant-goldfish-model"))),
            "got: {:?}",
            hits.iter().map(|h| &h.install_hint).collect::<Vec<_>>()
        );
    }

    #[test]
    fn merge_reserves_slots_for_github_over_weak_local() {
        let local = (0..5).map(|i| h(&format!("l{i}"), 0.6)).collect();
        let gh = (0..4).map(|i| h(&format!("g{i}"), 0.0)).collect();
        let names: Vec<_> = merge_github_hits(local, 5, gh)
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

    #[test]
    fn words_strip_qualifier_syntax() {
        assert_eq!(
            words("elephant  repo:evil/x \"goldfish\" -model"),
            ["elephant", "repoevilx", "goldfish", "-model"]
        );
        assert!(words(" : ").is_empty());
    }

    #[test]
    fn hit_prefers_frontmatter_name_and_description() {
        let md = "---\nname: egm-draft\ndescription: Drafts a design doc.\n---\nbody";
        let h = hit(
            "google/elephant-goldfish-model",
            "skills/egm-draft/SKILL.md",
            Some(md),
        );
        assert_eq!(h.name, "egm-draft");
        assert_eq!(h.description, "Drafts a design doc.");
        assert_eq!(h.source, "github");
        assert_eq!(
            h.install_hint.as_deref(),
            Some("gh skill install google/elephant-goldfish-model egm-draft")
        );
    }

    #[test]
    fn hit_falls_back_to_directory_name_without_frontmatter() {
        let h = hit("o/r", "skills/egm-review/SKILL.md", None);
        assert_eq!(h.name, "egm-review");
        assert_eq!(h.description, "");
        assert_eq!(skill_name("SKILL.md", None), "skill");
    }
}
