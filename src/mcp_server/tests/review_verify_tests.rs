use super::*;

fn created_item(s: &AgentflareMcp) -> String {
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Verify me"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(item_id.clone()),
        description: Some("Ship verify".into()),
        metadata: Some(serde_json::json!({
            "acceptance_criteria": [
                {"id": "ac1", "text": "empty pass demotes"},
                {"id": "ac2", "text": "fail stays fail"},
                {"id": "ac3", "text": "cited pass stays pass"}
            ]
        })),
        ..Default::default()
    }))
    .unwrap();
    item_id
}

fn verify(
    s: &AgentflareMcp,
    item_id: &str,
    criteria: Vec<VerifyCriterionInput>,
) -> Result<serde_json::Value, ErrorData> {
    let raw = s.review_impl(ReviewRequest {
        action: "verify".into(),
        item_id: Some(item_id.to_string()),
        criteria: Some(criteria),
        agent: Some("cursor".into()),
        ..Default::default()
    })?;
    Ok(serde_json::from_str(&raw).unwrap())
}

fn criterion(id: &str, verdict: &str, evidence: &str) -> VerifyCriterionInput {
    VerifyCriterionInput {
        id: id.to_string(),
        verdict: verdict.to_string(),
        evidence: evidence.to_string(),
    }
}

fn metadata_of(s: &AgentflareMcp, item_id: &str) -> serde_json::Value {
    let got: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "get".into(),
            id: Some(item_id.to_string()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    serde_json::from_str(got["metadata"].as_str().unwrap()).unwrap()
}

#[test]
fn review_verify_persists_demoted_pass_and_preserved_fail() {
    let (s, _tmp, _repo) = super::item_tests::claim_harness();
    let item_id = created_item(&s);
    let body = verify(
        &s,
        &item_id,
        vec![
            criterion("ac1", "pass", ""),
            criterion("ac2", "fail", ""),
            criterion("ac3", "pass", "src/review.rs:42"),
        ],
    )
    .unwrap();
    assert_eq!(body["criteria"][0]["verdict"], "uncertain");
    assert_eq!(body["criteria"][1]["verdict"], "fail");
    assert_eq!(body["criteria"][2]["verdict"], "pass");
    assert!(
        body["content_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(body.get("unchanged").is_none());

    let meta = metadata_of(&s, &item_id);
    assert_eq!(meta["verify"]["criteria"][0]["verdict"], "uncertain");
    assert_eq!(meta["verify"]["criteria"][1]["verdict"], "fail");
    assert_eq!(
        meta["verify"]["criteria"][2]["evidence"],
        "src/review.rs:42"
    );
    assert_eq!(meta["verify"]["evaluated_by"], "cursor");
    assert_eq!(meta["verify"]["content_hash"], body["content_hash"]);
    assert_eq!(meta["acceptance_criteria"][0]["id"], "ac1");
}

#[test]
fn review_verify_unchanged_content_hash_does_not_rewrite() {
    let (s, _tmp, _repo) = super::item_tests::claim_harness();
    let item_id = created_item(&s);
    let first = verify(
        &s,
        &item_id,
        vec![
            criterion("ac1", "pass", "src/review.rs:10"),
            criterion("ac2", "fail", ""),
            criterion("ac3", "uncertain", ""),
        ],
    )
    .unwrap();
    let second = verify(
        &s,
        &item_id,
        vec![
            criterion("ac1", "fail", "src/review.rs:99"),
            criterion("ac2", "pass", "src/review.rs:11"),
            criterion("ac3", "pass", "src/review.rs:12"),
        ],
    )
    .unwrap();
    assert_eq!(second["unchanged"], true);
    assert_eq!(second["criteria"][0]["verdict"], "pass");
    assert_eq!(second["criteria"][1]["verdict"], "fail");
    assert_eq!(second["content_hash"], first["content_hash"]);
    let meta = metadata_of(&s, &item_id);
    assert_eq!(meta["verify"]["criteria"][0]["verdict"], "pass");
    assert_eq!(
        meta["verify"]["criteria"][0]["evidence"],
        "src/review.rs:10"
    );
}

#[test]
fn review_verify_rejects_evidence_without_a_checkable_citation() {
    let (s, _tmp, _repo) = super::item_tests::claim_harness();
    let item_id = created_item(&s);
    let err = verify(
        &s,
        &item_id,
        vec![
            criterion("ac1", "pass", "the implementation looks correct"),
            criterion("ac2", "fail", ""),
            criterion("ac3", "fail", ""),
        ],
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("file:line"), "{msg}");
    assert!(msg.contains("test name"), "{msg}");
    assert!(msg.contains("command"), "{msg}");
    let meta = metadata_of(&s, &item_id);
    assert!(meta.get("verify").is_none());
}
