//! Per-criterion acceptance verdicts for `review(action=verify)`.
//!
//! The dispatched agent submits a verdict. Agentflare checks the citation
//! before trusting it — the same split as [`crate::review::consensus`], which
//! verifies a finding's citation before tagging it. A `pass` that cites
//! nothing is demoted to `uncertain`. A `fail` is kept.
//!
//! `content_hash` is the dirty-check latch, same shape as
//! `github::bridge::marker::content_hash` / `export_hash`: NUL-separated
//! sha256 over the item description and acceptance criteria. A matching
//! stored hash means the description and criteria are unchanged, so the
//! verdict is left alone (item #314's "same fingerprint, evidence still
//! stands" rule, applied to item content rather than the git tree).

use sha2::{Digest, Sha256};

/// What a checkable citation looks like. Returned when evidence is non-empty
/// prose with none of these shapes.
pub const CITATION_REQUIRED: &str = "evidence must cite something checkable: a file:line (src/foo.rs:42), a test name (`test_foo` or module::test_name), or a command with its output (`cargo test` → ok)";

const SOURCE_EXTS: &[&str] = &[
    "c", "cc", "cpp", "cs", "css", "go", "h", "hpp", "html", "java", "js", "jsx", "json", "kt",
    "md", "py", "rb", "rs", "sh", "sql", "svelte", "swift", "toml", "ts", "tsx", "txt", "vue",
    "xml", "yaml", "yml",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedCriterion {
    pub id: String,
    pub verdict: String,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VerifiedCriterion {
    pub id: String,
    pub verdict: String,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VerifyResult {
    pub criteria: Vec<VerifiedCriterion>,
    pub content_hash: String,
    /// Set only when the stored hash already matched and the verdict was not rewritten.
    #[serde(skip_serializing_if = "is_false")]
    pub unchanged: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(Debug)]
pub enum VerifyFailure {
    /// Caller input agentflare will not trust (bad verdict, uncited evidence).
    Rejected(String),
    /// Ledger read/write failed.
    Store(String),
}

/// Stable digest of the item description plus `acceptance_criteria`.
/// Criterion order does not change the digest; description and criterion
/// text do. Prefixed `sha256:` so the stored latch matches the design shape.
pub fn verify_content_hash(description: &str, acceptance_criteria: &serde_json::Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(description.as_bytes());
    hasher.update([0u8]);
    hasher.update(canonical_acceptance_criteria(acceptance_criteria).as_bytes());
    hasher.update([0u8]);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// Normalize submitted verdicts. Empty `pass` becomes `uncertain`. Empty
/// `fail` stays `fail`. Non-empty evidence with no checkable citation is
/// rejected for every verdict, including `fail`.
pub fn verify_criteria(criteria: &[SubmittedCriterion]) -> Result<Vec<VerifiedCriterion>, String> {
    if criteria.is_empty() {
        return Err("criteria is required".to_string());
    }
    criteria.iter().map(normalize_one).collect()
}

/// Persist `metadata.verify`, or return the stored verdict when the item's
/// description and acceptance criteria still hash to the stored latch.
/// The hash check runs before citation checks, so a re-submit against
/// unchanged content does not rewrite and does not reject.
/// A new write must name each acceptance-criterion id once: unknown,
/// duplicate, and missing ids are rejected and nothing is stored.
pub fn apply_item_verify(
    conn: &rusqlite::Connection,
    item_id: &str,
    criteria: &[SubmittedCriterion],
    evaluated_by: &str,
    now: i64,
) -> Result<VerifyResult, VerifyFailure> {
    let item = match agentflare_backend::item::get(conn, item_id) {
        Ok(item) => item,
        Err(agentflare_backend::Error::NotFound(_)) => {
            return Err(VerifyFailure::Rejected(format!(
                "no item matches id '{item_id}'"
            )));
        }
        Err(e) => return Err(VerifyFailure::Store(e.to_string())),
    };
    let acceptance = acceptance_criteria_value(&item.metadata);
    let hash = verify_content_hash(&item.description, &acceptance);
    if let Some(stored) = read_stored_verify(&item.metadata)
        && stored.content_hash == hash
    {
        return Ok(VerifyResult {
            criteria: stored.criteria,
            content_hash: hash,
            unchanged: true,
        });
    }
    let known = known_criterion_ids(&acceptance).map_err(VerifyFailure::Rejected)?;
    require_complete_ids(criteria, &known).map_err(VerifyFailure::Rejected)?;
    let verified = verify_criteria(criteria).map_err(VerifyFailure::Rejected)?;
    let patch = serde_json::json!({
        "verify": {
            "evaluated_at": now,
            "evaluated_by": evaluated_by,
            "content_hash": hash,
            "criteria": verified,
        }
    });
    let metadata = agentflare_backend::item::plan_gate::merge_metadata_patch(&item.metadata, patch);
    agentflare_backend::item::update(
        conn,
        item_id,
        agentflare_backend::item::UpdateItem {
            metadata: Some(metadata),
            ..Default::default()
        },
    )
    .map_err(|e| VerifyFailure::Store(e.to_string()))?;
    Ok(VerifyResult {
        criteria: verified,
        content_hash: hash,
        unchanged: false,
    })
}

/// Completion gate for an enforced item: `None` when every acceptance
/// criterion has a current `pass` verdict with evidence, else a per-criterion
/// report (criterion, verdict, evidence) explaining why a human is needed.
/// A verdict whose content hash no longer matches the item is stale.
pub fn completion_gate(metadata: &str, description: &str) -> Option<String> {
    let acceptance = acceptance_criteria_value(metadata);
    let Ok(known) = known_criterion_ids(&acceptance) else {
        return Some(
            "item has no acceptance_criteria, so nothing can be verified. Add criteria, then run review(action=verify).".into(),
        );
    };
    let stored = read_stored_verify(metadata)
        .filter(|v| v.content_hash == verify_content_hash(description, &acceptance));
    let Some(stored) = stored else {
        return Some(
            "no current verification verdict for this item's description and acceptance_criteria. Run review(action=verify).".into(),
        );
    };
    let mut blocked = false;
    let mut lines = Vec::new();
    for id in &known {
        let (verdict, evidence) = stored
            .criteria
            .iter()
            .find(|c| c.id == *id)
            .map(|c| (c.verdict.as_str(), c.evidence.as_str()))
            .unwrap_or(("uncertain", ""));
        blocked |= verdict != "pass" || evidence.trim().is_empty();
        let evidence = if evidence.trim().is_empty() {
            "(none)"
        } else {
            evidence
        };
        lines.push(format!("- {id}: {verdict} — evidence: {evidence}"));
    }
    blocked.then(|| lines.join("\n"))
}

fn acceptance_criteria_value(metadata: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(metadata)
        .ok()
        .and_then(|v| v.get("acceptance_criteria").cloned())
        .unwrap_or(serde_json::Value::Null)
}

struct StoredVerify {
    content_hash: String,
    criteria: Vec<VerifiedCriterion>,
}

fn read_stored_verify(metadata: &str) -> Option<StoredVerify> {
    let value: serde_json::Value = serde_json::from_str(metadata).ok()?;
    let verify = value.get("verify")?;
    let content_hash = verify.get("content_hash")?.as_str()?.to_string();
    let criteria = serde_json::from_value(verify.get("criteria")?.clone()).ok()?;
    Some(StoredVerify {
        content_hash,
        criteria,
    })
}

fn canonical_acceptance_criteria(value: &serde_json::Value) -> String {
    let Some(arr) = value.as_array() else {
        return format!("raw\0{value}");
    };
    let mut rows: Vec<String> = arr.iter().map(canonical_criterion).collect();
    rows.sort();
    let mut out = String::new();
    for row in rows {
        out.push_str(&row);
        out.push('\0');
    }
    out
}

/// `c\0id\0text` only for an object whose fields are exactly string `id` and
/// `text`. Every other element is its JSON, so a field the normalized form
/// would drop still moves the latch.
fn canonical_criterion(value: &serde_json::Value) -> String {
    let Some(obj) = value.as_object() else {
        return format!("r\0{value}");
    };
    match (
        obj.len() == 2,
        obj.get("id").and_then(|v| v.as_str()),
        obj.get("text").and_then(|v| v.as_str()),
    ) {
        (true, Some(id), Some(text)) => format!("c\0{id}\0{text}"),
        _ => format!("r\0{value}"),
    }
}

fn known_criterion_ids(
    acceptance: &serde_json::Value,
) -> Result<std::collections::BTreeSet<String>, String> {
    if acceptance.is_null() {
        return Err("item has no acceptance_criteria".to_string());
    }
    let Some(arr) = acceptance.as_array() else {
        return Err("acceptance_criteria must be an array of {id, text}".to_string());
    };
    if arr.is_empty() {
        return Err("item has no acceptance_criteria".to_string());
    }
    let mut ids = std::collections::BTreeSet::new();
    for c in arr {
        let Some(id) = c
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Err("acceptance_criteria entry is missing a string id".to_string());
        };
        if !ids.insert(id.to_string()) {
            return Err(format!("acceptance_criteria contains duplicate id '{id}'"));
        }
    }
    Ok(ids)
}

fn require_complete_ids(
    criteria: &[SubmittedCriterion],
    known: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for c in criteria {
        let id = c.id.trim();
        if id.is_empty() {
            return Err("criterion id is required".to_string());
        }
        if !known.contains(id) {
            let known_ids = known
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "unknown criterion id '{id}'; known ids are {known_ids}"
            ));
        }
        if !seen.insert(id.to_string()) {
            return Err(format!("duplicate criterion id '{id}'"));
        }
    }
    let missing: Vec<&str> = known
        .iter()
        .filter(|id| !seen.contains(id.as_str()))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "missing verdict for criterion '{}'",
            missing.join("', '")
        ));
    }
    Ok(())
}

fn normalize_one(c: &SubmittedCriterion) -> Result<VerifiedCriterion, String> {
    let id = c.id.trim();
    if id.is_empty() {
        return Err("criterion id is required".to_string());
    }
    let verdict = c.verdict.trim().to_ascii_lowercase();
    let evidence = c.evidence.trim();
    let kind = match verdict.as_str() {
        "pass" | "fail" | "uncertain" => verdict.as_str(),
        other => {
            return Err(format!(
                "criterion '{id}': verdict must be pass, fail, or uncertain (got {other})"
            ));
        }
    };
    if evidence.is_empty() {
        let verdict = if kind == "pass" { "uncertain" } else { kind };
        return Ok(VerifiedCriterion {
            id: id.to_string(),
            verdict: verdict.to_string(),
            evidence: String::new(),
        });
    }
    if !evidence_has_citation(evidence) {
        return Err(format!("criterion '{id}': {CITATION_REQUIRED}"));
    }
    Ok(VerifiedCriterion {
        id: id.to_string(),
        verdict: kind.to_string(),
        evidence: evidence.to_string(),
    })
}

fn evidence_has_citation(evidence: &str) -> bool {
    has_file_line(evidence) || has_test_name(evidence) || has_command_with_output(evidence)
}

fn has_file_line(evidence: &str) -> bool {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?:^|[\s`'"(])((?:[\w.@+-]+/)*[\w.@+-]+)\.([A-Za-z0-9]+):([0-9]+)"#)
            .expect("file:line citation regex")
    });
    RE.captures_iter(evidence).any(|c| {
        let path = c.get(1).map(|m| m.as_str()).unwrap_or("");
        let ext = c.get(2).map(|m| m.as_str()).unwrap_or("");
        path.contains('/') || SOURCE_EXTS.contains(&ext)
    })
}

fn has_test_name(evidence: &str) -> bool {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r#"`(?:test_[A-Za-z0-9_]+|[A-Za-z_][A-Za-z0-9_]*::[A-Za-z0-9_:]+)`|(?:^|[\s`'"(])(?:[A-Za-z_][A-Za-z0-9_]*::)+[A-Za-z_][A-Za-z0-9_]*|(?i)(?:^|\s)tests?\s+[A-Za-z_][A-Za-z0-9_:]*[_:][A-Za-z0-9_:]*"#,
        )
        .expect("test-name citation regex")
    });
    RE.is_match(evidence)
}

fn has_command_with_output(evidence: &str) -> bool {
    if let Some((head, tail)) = evidence.split_once('\n') {
        return looks_like_command(head) && !tail.trim().is_empty();
    }
    for sep in ["→", "->", " — ", " -- "] {
        if let Some((head, tail)) = evidence.split_once(sep)
            && looks_like_command(head)
            && !tail.trim().is_empty()
        {
            return true;
        }
    }
    false
}

fn looks_like_command(line: &str) -> bool {
    let trimmed = line.trim().trim_matches('`');
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("./") || lower.starts_with("../") {
        return true;
    }
    const HEADS: &[&str] = &[
        "cargo", "npm", "pnpm", "yarn", "bun", "pytest", "mvn", "gradle", "ctest", "rustc", "git",
    ];
    let Some((head, rest)) = lower.split_once(|c: char| c.is_whitespace()) else {
        return HEADS.contains(&lower.as_str());
    };
    if head == "go" {
        return rest.starts_with("test");
    }
    HEADS.contains(&head)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(id: &str, verdict: &str, evidence: &str) -> SubmittedCriterion {
        SubmittedCriterion {
            id: id.to_string(),
            verdict: verdict.to_string(),
            evidence: evidence.to_string(),
        }
    }

    fn seed(conn: &rusqlite::Connection, description: &str, criteria: serde_json::Value) -> String {
        let ws = agentflare_backend::workspace::create(
            conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "Verify".into(),
                slug: "verify".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let proj = agentflare_backend::project::create(
            conn,
            agentflare_backend::project::CreateProject {
                workspace_id: ws.id,
                name: "Verify".into(),
                identifier: "VERIFY".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let state_id = agentflare_backend::state::list_by_project(conn, &proj.id)
            .unwrap()
            .into_iter()
            .find(|s| s.is_default)
            .unwrap()
            .id;
        let id = agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: proj.id,
                state_id,
                name: "Verify me".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
                start_date: None,
                due_date: None,
            },
        )
        .unwrap()
        .id;
        let metadata = serde_json::json!({
            "size": "S",
            "acceptance_criteria": criteria,
        })
        .to_string();
        agentflare_backend::item::update(
            conn,
            &id,
            agentflare_backend::item::UpdateItem {
                description: Some(description.to_string()),
                metadata: Some(metadata),
                ..Default::default()
            },
        )
        .unwrap();
        id
    }

    #[test]
    fn pass_with_empty_evidence_is_demoted_to_uncertain() {
        let out = verify_criteria(&[sub("ac1", "pass", "  ")]).unwrap();
        assert_eq!(out[0].verdict, "uncertain");
        assert_eq!(out[0].evidence, "");
    }

    #[test]
    fn uncertain_with_no_evidence_stays_uncertain() {
        let out = verify_criteria(&[sub("ac1", "uncertain", "")]).unwrap();
        assert_eq!(out[0].verdict, "uncertain");
    }

    #[test]
    fn fail_is_preserved_even_when_another_criterion_passes() {
        let out = verify_criteria(&[
            sub("ac1", "fail", ""),
            sub("ac2", "pass", "src/review.rs:42"),
        ])
        .unwrap();
        assert_eq!(out[0].verdict, "fail");
        assert_eq!(out[0].id, "ac1");
        assert_eq!(out[1].verdict, "pass");
    }

    #[test]
    fn cited_file_line_test_name_and_command_are_accepted() {
        let out = verify_criteria(&[
            sub("ac1", "pass", "item.rs:2141 — passes"),
            sub("ac2", "pass", "`test_foo`"),
            sub("ac3", "pass", "module::test_name"),
            sub("ac4", "uncertain", "cargo test verify_criteria\nok"),
        ])
        .unwrap();
        assert_eq!(
            out.iter().map(|c| c.verdict.as_str()).collect::<Vec<_>>(),
            ["pass", "pass", "pass", "uncertain"]
        );
    }

    #[test]
    fn prose_without_a_citation_is_rejected_with_the_citation_shape() {
        for evidence in [
            "the code looks fine and the tests pass",
            "https://example.com:443",
            "cargo test",
        ] {
            let err = verify_criteria(&[sub("ac1", "pass", evidence)]).unwrap_err();
            assert!(err.contains("file:line"), "{evidence}: {err}");
            assert!(err.contains("test name"), "{evidence}: {err}");
            assert!(err.contains("command"), "{evidence}: {err}");
        }
        let err = verify_criteria(&[sub("ac1", "fail", "this just looks wrong")]).unwrap_err();
        assert!(err.contains(CITATION_REQUIRED), "{err}");
    }

    #[test]
    fn content_hash_is_stable_and_tracks_description_and_criteria() {
        let criteria = serde_json::json!([
            {"id": "b", "text": "two"},
            {"id": "a", "text": "one"}
        ]);
        let reordered = serde_json::json!([
            {"id": "a", "text": "one"},
            {"id": "b", "text": "two"}
        ]);
        let base = verify_content_hash("body", &criteria);
        assert_eq!(base, verify_content_hash("body", &reordered));
        assert!(base.starts_with("sha256:"));
        assert_ne!(base, verify_content_hash("Body", &criteria));
        assert_ne!(
            base,
            verify_content_hash("body", &serde_json::json!([{"id": "a", "text": "changed"}]))
        );
        assert_ne!(
            verify_content_hash("ab", &serde_json::json!([{"id": "c", "text": ""}])),
            verify_content_hash("a", &serde_json::json!([{"id": "bc", "text": ""}]))
        );
    }

    #[test]
    fn unchanged_content_hash_does_not_rewrite_the_verdict() {
        let conn = agentflare_backend::open_in_memory().unwrap();
        let id = seed(
            &conn,
            "ship it",
            serde_json::json!([{"id": "ac1", "text": "persists"}]),
        );
        let first = apply_item_verify(
            &conn,
            &id,
            &[sub("ac1", "pass", "src/review.rs:10")],
            "agent-a",
            100,
        )
        .unwrap();
        assert!(!first.unchanged);
        assert_eq!(first.criteria[0].verdict, "pass");

        let second = apply_item_verify(
            &conn,
            &id,
            &[sub("ac1", "fail", "no citation here")],
            "agent-b",
            999,
        )
        .unwrap();
        assert!(second.unchanged);
        assert_eq!(second.criteria[0].verdict, "pass");
        assert_eq!(second.content_hash, first.content_hash);

        let item = agentflare_backend::item::get(&conn, &id).unwrap();
        let meta: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
        assert_eq!(meta["verify"]["evaluated_at"], 100);
        assert_eq!(meta["verify"]["evaluated_by"], "agent-a");
        assert_eq!(meta["verify"]["criteria"][0]["verdict"], "pass");
        assert_eq!(meta["verify"]["content_hash"], first.content_hash);
        assert_eq!(meta["acceptance_criteria"][0]["id"], "ac1");
        assert_eq!(meta["size"], "S");

        agentflare_backend::item::update(
            &conn,
            &id,
            agentflare_backend::item::UpdateItem {
                description: Some("ship it, revised".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let third = apply_item_verify(
            &conn,
            &id,
            &[sub("ac1", "fail", "src/review.rs:11")],
            "agent-c",
            200,
        )
        .unwrap();
        assert!(!third.unchanged);
        assert_eq!(third.criteria[0].verdict, "fail");
        assert_ne!(third.content_hash, first.content_hash);
    }

    #[test]
    fn rejected_evidence_does_not_persist() {
        let conn = agentflare_backend::open_in_memory().unwrap();
        let id = seed(
            &conn,
            "ship it",
            serde_json::json!([{"id": "ac1", "text": "persists"}]),
        );
        let err = apply_item_verify(
            &conn,
            &id,
            &[sub("ac1", "pass", "looks good")],
            "agent-a",
            100,
        )
        .unwrap_err();
        assert!(matches!(err, VerifyFailure::Rejected(_)));
        let item = agentflare_backend::item::get(&conn, &id).unwrap();
        let meta: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
        assert!(meta.get("verify").is_none());
    }

    #[test]
    fn content_hash_changes_when_a_criterion_is_not_exactly_id_and_text() {
        let plain = serde_json::json!([{"id": "a", "text": "one"}]);
        let reordered_keys = serde_json::json!([{"text": "one", "id": "a"}]);
        let extra = serde_json::json!([{"id": "a", "text": "one", "note": "x"}]);
        let other_extra = serde_json::json!([{"id": "a", "text": "one", "note": "y"}]);
        let base = verify_content_hash("body", &plain);
        assert_eq!(base, verify_content_hash("body", &reordered_keys));
        assert_ne!(base, verify_content_hash("body", &extra));
        assert_ne!(
            verify_content_hash("body", &extra),
            verify_content_hash("body", &other_extra)
        );
        assert_ne!(
            verify_content_hash("body", &serde_json::json!([{"id": "", "text": ""}])),
            verify_content_hash("body", &serde_json::json!(["not-an-object"]))
        );
        assert_ne!(
            verify_content_hash("body", &serde_json::json!([{"nope": 1}])),
            verify_content_hash("body", &serde_json::json!([{"nope": 2}]))
        );
    }

    #[test]
    fn unknown_duplicate_and_partial_ids_are_rejected_without_persisting() {
        let conn = agentflare_backend::open_in_memory().unwrap();
        let id = seed(
            &conn,
            "ship it",
            serde_json::json!([
                {"id": "ac1", "text": "one"},
                {"id": "ac2", "text": "two"}
            ]),
        );
        let cited = "src/review.rs:10";
        let unknown = apply_item_verify(
            &conn,
            &id,
            &[sub("ac9", "pass", cited), sub("ac1", "fail", "")],
            "agent-a",
            100,
        )
        .unwrap_err();
        assert!(
            matches!(unknown, VerifyFailure::Rejected(ref m) if m.contains("unknown criterion id 'ac9'")),
            "{unknown:?}"
        );
        let duplicate = apply_item_verify(
            &conn,
            &id,
            &[
                sub("ac1", "pass", cited),
                sub("ac1", "fail", ""),
                sub("ac2", "fail", ""),
            ],
            "agent-a",
            100,
        )
        .unwrap_err();
        assert!(
            matches!(duplicate, VerifyFailure::Rejected(ref m) if m.contains("duplicate criterion id 'ac1'")),
            "{duplicate:?}"
        );
        let partial = apply_item_verify(&conn, &id, &[sub("ac1", "pass", cited)], "agent-a", 100)
            .unwrap_err();
        assert!(
            matches!(partial, VerifyFailure::Rejected(ref m) if m.contains("missing verdict") && m.contains("ac2")),
            "{partial:?}"
        );
        let item = agentflare_backend::item::get(&conn, &id).unwrap();
        let meta: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
        assert!(meta.get("verify").is_none());
    }

    #[test]
    fn extra_criterion_field_invalidates_the_stored_latch() {
        let conn = agentflare_backend::open_in_memory().unwrap();
        let id = seed(
            &conn,
            "ship it",
            serde_json::json!([{"id": "ac1", "text": "persists"}]),
        );
        let first = apply_item_verify(
            &conn,
            &id,
            &[sub("ac1", "pass", "src/review.rs:10")],
            "agent-a",
            100,
        )
        .unwrap();
        assert!(!first.unchanged);
        let item = agentflare_backend::item::get(&conn, &id).unwrap();
        let mut meta: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
        meta["acceptance_criteria"][0]["note"] = serde_json::json!("revised");
        agentflare_backend::item::update(
            &conn,
            &id,
            agentflare_backend::item::UpdateItem {
                metadata: Some(meta.to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        let second = apply_item_verify(
            &conn,
            &id,
            &[sub("ac1", "fail", "src/review.rs:11")],
            "agent-b",
            200,
        )
        .unwrap();
        assert!(!second.unchanged);
        assert_eq!(second.criteria[0].verdict, "fail");
        assert_ne!(second.content_hash, first.content_hash);
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    fn meta(verdict: &str, evidence: &str) -> (String, String) {
        let ac = serde_json::json!([{"id": "a", "text": "t"}]);
        let hash = verify_content_hash("d", &ac);
        let m = serde_json::json!({"acceptance_criteria": ac, "verify": {"content_hash": hash,
            "criteria": [{"id": "a", "verdict": verdict, "evidence": evidence}]}});
        (m.to_string(), "d".into())
    }

    #[test]
    fn gate_blocks_everything_but_cited_pass() {
        let (m, d) = meta("pass", "src/a.rs:1");
        assert!(completion_gate(&m, &d).is_none());
        for (v, e) in [("fail", "src/a.rs:1"), ("uncertain", "x"), ("pass", "")] {
            let (m, d) = meta(v, e);
            let r = completion_gate(&m, &d).unwrap();
            assert!(r.contains("- a:"), "{r}");
        }
        assert!(
            completion_gate("{}", "d")
                .unwrap()
                .contains("no acceptance_criteria")
        );
        let (m, _) = meta("pass", "src/a.rs:1");
        assert!(
            completion_gate(&m, "changed")
                .unwrap()
                .contains("no current")
        );
    }
}
