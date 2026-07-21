use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TaskType {
    Generate,
    FixBug,
    Refactor,
    Explore,
    Test,
    Debug,
    Config,
    Deploy,
    Review,
}

impl TaskType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Generate => "generate",
            Self::FixBug => "fix_bug",
            Self::Refactor => "refactor",
            Self::Explore => "explore",
            Self::Test => "test",
            Self::Debug => "debug",
            Self::Config => "config",
            Self::Deploy => "deploy",
            Self::Review => "review",
        }
    }

    pub fn search_queries(&self) -> &[&str] {
        match self {
            Self::Generate => &["create", "implement", "add", "new", "build", "write"],
            Self::FixBug => &["fix", "bug", "error", "crash", "broken", "debug"],
            Self::Refactor => &["refactor", "clean", "restructure", "simplify", "rename"],
            Self::Explore => &[
                "explain",
                "understand",
                "how",
                "what",
                "find",
                "where",
                "show",
            ],
            Self::Test => &["test", "spec", "coverage", "assert", "mock"],
            Self::Debug => &["debug", "trace", "inspect", "log", "diagnose"],
            Self::Config => &["config", "setup", "install", "configure", "env"],
            Self::Deploy => &["deploy", "release", "publish", "ci", "pipeline"],
            Self::Review => &["review", "audit", "check", "evaluate", "assess"],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct IntentClassification {
    pub task_type: TaskType,
    pub confidence: f64,
    pub keywords: Vec<String>,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RankedSkill {
    pub name: String,
    pub source: String,
    pub description: String,
    pub score: f64,
    pub match_reason: String,
}

const PHRASE_RULES: &[(&[&str], TaskType, f64)] = &[
    (
        &[
            "add",
            "create",
            "implement",
            "build",
            "write",
            "generate",
            "new feature",
        ],
        TaskType::Generate,
        0.9,
    ),
    (
        &[
            "fix",
            "bug",
            "broken",
            "crash",
            "error in",
            "not working",
            "fails",
            "wrong output",
        ],
        TaskType::FixBug,
        0.95,
    ),
    (
        &[
            "refactor",
            "clean up",
            "restructure",
            "rename",
            "move",
            "extract",
            "simplify",
            "split",
        ],
        TaskType::Refactor,
        0.9,
    ),
    (
        &[
            "how",
            "what",
            "where",
            "explain",
            "understand",
            "show me",
            "describe",
            "why does",
        ],
        TaskType::Explore,
        0.85,
    ),
    (
        &[
            "test",
            "spec",
            "coverage",
            "assert",
            "unit test",
            "integration test",
            "mock",
        ],
        TaskType::Test,
        0.9,
    ),
    (
        &[
            "debug",
            "trace",
            "inspect",
            "log",
            "breakpoint",
            "step through",
            "stack trace",
        ],
        TaskType::Debug,
        0.9,
    ),
    (
        &[
            "config",
            "setup",
            "install",
            "env",
            "configure",
            "settings",
            "dotenv",
        ],
        TaskType::Config,
        0.85,
    ),
    (
        &[
            "deploy", "release", "publish", "ship", "ci/cd", "pipeline", "docker",
        ],
        TaskType::Deploy,
        0.85,
    ),
    (
        &[
            "review",
            "check",
            "audit",
            "look at",
            "evaluate",
            "assess",
            "pr review",
        ],
        TaskType::Review,
        0.8,
    ),
];

pub fn classify(query: &str) -> IntentClassification {
    let q = query.to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();

    let mut best_type = TaskType::Explore;
    let mut best_score = 0.0_f64;

    for &(phrases, task_type, base_confidence) in PHRASE_RULES {
        let mut match_count = 0usize;
        for phrase in phrases {
            if phrase.contains(' ') {
                if q.contains(phrase) {
                    match_count += 2;
                }
            } else if words.iter().any(|w| w == phrase) {
                match_count += 1;
            }
        }
        if match_count > 0 {
            let score =
                base_confidence * (0.8 + 0.2 * ((match_count as f64 - 1.0).clamp(0.0, 1.0)));
            if score > best_score {
                best_score = score;
                best_type = task_type;
            }
        }
    }

    let keywords = extract_keywords(&q);
    let targets = extract_targets(query);

    if best_score < 0.1 {
        best_type = TaskType::Explore;
        best_score = 0.3;
    }

    IntentClassification {
        task_type: best_type,
        confidence: best_score,
        keywords,
        targets,
    }
}

fn extract_keywords(query: &str) -> Vec<String> {
    let stopwords = [
        "the", "this", "that", "with", "from", "into", "have", "please", "could", "would",
        "should", "also", "just", "then", "when", "what", "where", "which", "there", "here",
        "these", "those", "does", "will", "shall", "can", "may", "must", "need", "want", "like",
        "make", "take", "and", "for", "not", "are", "was", "but", "all", "some", "any",
    ];
    query
        .split_whitespace()
        .filter(|w| w.len() > 3)
        .filter(|w| !stopwords.contains(w))
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .take(8)
        .collect()
}

fn extract_targets(query: &str) -> Vec<String> {
    let mut targets = Vec::new();
    for word in query.split_whitespace() {
        if word.contains('.') && !word.starts_with('.') {
            let clean = word.trim_matches(|c: char| {
                !c.is_alphanumeric() && c != '.' && c != '/' && c != '_' && c != '-'
            });
            if looks_like_path(clean) {
                targets.push(clean.to_string());
            }
        }
        if word.contains('/') && !word.starts_with("//") && !word.starts_with("http") {
            let clean = word.trim_matches(|c: char| {
                !c.is_alphanumeric() && c != '.' && c != '/' && c != '_' && c != '-'
            });
            if clean.len() > 2 {
                targets.push(clean.to_string());
            }
        }
    }
    targets.truncate(5);
    targets
}

fn looks_like_path(s: &str) -> bool {
    let exts = [
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".toml", ".yaml", ".yml", ".json", ".md",
    ];
    exts.iter().any(|ext| s.ends_with(ext)) || s.contains('/')
}

pub fn find_skills(
    intent: &IntentClassification,
    registry: &skill_registry::Registry,
    limit: usize,
    embed_query: impl Fn(&str) -> Option<Vec<f32>>,
    embed_doc: impl Fn(&str) -> Option<Vec<f32>>,
) -> Result<Vec<RankedSkill>, String> {
    let mut seen = HashMap::new();
    let mut results = Vec::new();

    // Phase 1: search by TaskType-derived queries
    let queries = intent.task_type.search_queries();
    for query_prefix in queries.iter().take(2) {
        let query = format!("{} {}", query_prefix, intent.keywords.join(" "));
        if query.trim().len() < 3 {
            continue;
        }
        if let Ok(hits) = registry.search(&query, limit, skill_registry::MatchMode::Any) {
            for hit in hits {
                if seen.contains_key(&hit.name) {
                    continue;
                }
                let score = hit.score * intent.confidence;
                let reason = format!("matches {} task", intent.task_type.as_str());
                seen.insert(hit.name.clone(), ());
                results.push(RankedSkill {
                    name: hit.name,
                    source: hit.source,
                    description: hit.description,
                    score,
                    match_reason: reason,
                });
            }
        }
    }

    // Phase 2: search by raw keywords
    if results.len() < limit {
        let keyword_q = intent.keywords.join(" ");
        if keyword_q.len() >= 3
            && let Ok(hits) = registry.search(
                &keyword_q,
                limit - results.len(),
                skill_registry::MatchMode::Any,
            )
        {
            for hit in hits {
                if seen.contains_key(&hit.name) {
                    continue;
                }
                let score = hit.score * 0.6;
                let reason = "keyword match".to_string();
                seen.insert(hit.name.clone(), ());
                results.push(RankedSkill {
                    name: hit.name,
                    source: hit.source,
                    description: hit.description,
                    score,
                    match_reason: reason,
                });
            }
        }
    }

    // Semantic re-rank: blend cosine similarity against each candidate's
    // description into the BM25 score, same 0.4/0.6 weighting as
    // memory::search::search_hybrid uses. `embed_query` returning `None`
    // (no `semantic` feature, no downloaded model) leaves `results` exactly
    // as BM25 ranked it -- byte-identical to the pre-embedding behavior.
    let query_text = format!(
        "{} {}",
        intent.task_type.as_str(),
        intent.keywords.join(" ")
    );
    if let Some(qvec) = embed_query(&query_text) {
        let doc_vecs: HashMap<String, Vec<f32>> = results
            .iter()
            .filter_map(|s| embed_doc(&s.description).map(|v| (s.name.clone(), v)))
            .collect();
        rerank_with_embeddings(&mut results, &qvec, &doc_vecs);
    }

    // Sort by score descending
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(limit);
    Ok(results)
}

/// Blend BM25 rank with cosine-similarity semantic rank (0.4/0.6) for
/// candidates that have a doc embedding; a candidate with no embedding
/// keeps its BM25-only score untouched.
fn rerank_with_embeddings(
    results: &mut [RankedSkill],
    qvec: &[f32],
    doc_vecs: &HashMap<String, Vec<f32>>,
) {
    let bm25_pairs: Vec<(String, f64)> =
        results.iter().map(|s| (s.name.clone(), s.score)).collect();
    let vec_pairs: Vec<(String, f64)> = results
        .iter()
        .filter_map(|s| {
            let dvec = doc_vecs.get(&s.name)?;
            let sim = agentflare_store::embed::cosine_similarity(qvec, dvec)? as f64;
            Some((s.name.clone(), sim))
        })
        .collect();
    if vec_pairs.is_empty() {
        return;
    }
    let merged = agentflare_store::retrieval::merge_ranked(&bm25_pairs, &vec_pairs, 0.4, 0.6);
    let score_by_name: HashMap<String, f64> = merged.into_iter().collect();
    for r in results.iter_mut() {
        if let Some(&s) = score_by_name.get(&r.name) {
            r.score = s;
        }
    }
}

pub fn build_injection(skills: &[RankedSkill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    lines.push("-- relevant skills --".to_string());
    for s in skills {
        lines.push(format!(
            "- {}: {} (confidence {:.0}%)",
            s.name,
            s.description,
            s.score * 100.0
        ));
    }
    lines.push("Use skill_search(name) for details then skill_load(name) to inject.".to_string());
    Some(lines.join("\n"))
}

pub fn session_context_queries() -> Vec<String> {
    let cwd = std::env::current_dir().ok();
    let mut queries = Vec::new();

    if let Some(dir) = cwd {
        if dir.join("Cargo.toml").exists() {
            queries.push("rust cargo".to_string());
        }
        if dir.join("package.json").exists() {
            queries.push("node typescript".to_string());
        }
        if dir.join("pyproject.toml").exists() || dir.join("requirements.txt").exists() {
            queries.push("python".to_string());
        }
    }

    queries
}

/// Inject intent classification header into system prompt.
pub fn format_briefing_header(intent: &IntentClassification) -> String {
    format!(
        "[INTENT:{} CONF:{:.0}% KW:{}]",
        intent.task_type.as_str(),
        intent.confidence * 100.0,
        if intent.keywords.is_empty() {
            "-".to_string()
        } else {
            intent.keywords.join(",")
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_fix_bug() {
        let r = classify("fix the bug in auth.rs where login returns 500");
        assert_eq!(r.task_type, TaskType::FixBug);
        assert!(r.confidence > 0.5);
    }

    #[test]
    fn classify_generate() {
        let r = classify("add a new login endpoint to auth.rs");
        assert_eq!(r.task_type, TaskType::Generate);
        assert!(r.confidence > 0.5);
    }

    #[test]
    fn classify_refactor() {
        let r = classify("refactor the auth module into smaller pieces");
        assert_eq!(r.task_type, TaskType::Refactor);
    }

    #[test]
    fn classify_explore() {
        let r = classify("how does authentication work?");
        assert_eq!(r.task_type, TaskType::Explore);
    }

    #[test]
    fn classify_review() {
        let r = classify("review this PR for security issues");
        assert_eq!(r.task_type, TaskType::Review);
    }

    #[test]
    fn classify_test() {
        let r = classify("write unit tests for the auth module");
        assert_eq!(r.task_type, TaskType::Test);
    }

    #[test]
    fn classify_debug() {
        let r = classify("debug why login returns 500");
        assert_eq!(r.task_type, TaskType::Debug);
    }

    #[test]
    fn classify_deploy() {
        let r = classify("deploy the new version to production");
        assert_eq!(r.task_type, TaskType::Deploy);
    }

    #[test]
    fn classify_config() {
        let r = classify("configure environment variables for the API");
        assert_eq!(r.task_type, TaskType::Config);
    }

    #[test]
    fn fallback_to_explore() {
        let r = classify("xyz qqq bbb");
        assert_eq!(r.task_type, TaskType::Explore);
        assert!(r.confidence < 0.5);
    }

    #[test]
    fn extract_targets_paths() {
        let r = classify("fix entropy.rs and update core/mod.rs");
        assert!(r.targets.iter().any(|t| t.contains("entropy.rs")));
        assert!(r.targets.iter().any(|t| t.contains("core/mod.rs")));
    }

    #[test]
    fn brief_header_format() {
        let r = classify("fix the bug in auth.rs");
        let h = format_briefing_header(&r);
        assert!(h.contains("fix_bug"));
        assert!(h.contains("CONF"));
    }

    #[test]
    fn build_injection_empty() {
        assert!(build_injection(&[]).is_none());
    }

    #[test]
    fn build_injection_lists_skills() {
        let skills = vec![RankedSkill {
            name: "test-driven-development".into(),
            source: "superpowers".into(),
            description: "Use before writing implementation code".into(),
            score: 0.87,
            match_reason: "matches test task".into(),
        }];
        let out = build_injection(&skills).unwrap();
        assert!(out.contains("test-driven-development"));
        assert!(out.contains("87%"));
    }

    fn ranked(name: &str, score: f64) -> RankedSkill {
        RankedSkill {
            name: name.to_string(),
            source: "s".into(),
            description: "d".into(),
            score,
            match_reason: "bm25".into(),
        }
    }

    #[test]
    fn rerank_with_embeddings_boosts_semantic_match_over_bm25_leader() {
        // "a" leads on BM25 alone; "b" is the true semantic match (identical
        // vector to the query) -- the blend should promote it above "a".
        let mut results = vec![ranked("a", 0.9), ranked("b", 0.1)];
        let qvec = vec![1.0_f32, 0.0];
        let mut doc_vecs = HashMap::new();
        doc_vecs.insert("a".to_string(), vec![0.0_f32, 1.0]);
        doc_vecs.insert("b".to_string(), vec![1.0_f32, 0.0]);
        rerank_with_embeddings(&mut results, &qvec, &doc_vecs);
        let a = results.iter().find(|r| r.name == "a").unwrap();
        let b = results.iter().find(|r| r.name == "b").unwrap();
        assert!(
            b.score > a.score,
            "expected semantic match to outrank bm25 leader: a={} b={}",
            a.score,
            b.score
        );
    }

    #[test]
    fn rerank_with_embeddings_noop_without_doc_vectors() {
        let mut results = vec![ranked("a", 0.42)];
        rerank_with_embeddings(&mut results, &[1.0, 0.0], &HashMap::new());
        assert_eq!(results[0].score, 0.42);
    }
}
