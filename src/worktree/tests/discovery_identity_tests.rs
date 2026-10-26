use super::tests::{create_owner_item, test_project_with_in_review_state};
use super::*;
use crate::github::{
    RepoId,
    test_support::{MockResponse, MockServer},
};

fn unstamped_pr(branch: &str, title: &str) -> crate::github::models::PullRequest {
    serde_json::from_value(serde_json::json!({
        "number": 870, "html_url": "u", "state": "open", "title": title,
        "head": {"ref": branch}, "author_association": "OWNER"
    }))
    .unwrap()
}

#[test]
fn item724_discovered_identity_survives_title_changes_without_trusting_conflicting_tags() {
    let conn = agentflare_backend::db::open_in_memory().unwrap();
    let (project, state) = test_project_with_in_review_state(&conn);
    let mut pr = unstamped_pr("fix/hand-opened", "Original title");
    let item = create_tracking_item(&conn, &project, &state, &pr, "fix/hand-opened").unwrap();
    pr.title = "Edited title".into();
    assert!(super::super::pr_item_mismatch(&item, &pr).is_none());
    pr.body = Some(crate::github::pulls::item_id_tag("another-item"));
    assert!(super::super::pr_item_mismatch(&item, &pr).is_some());
    pr.body = None;
    pr.number = 871;
    assert!(super::super::pr_item_mismatch(&item, &pr).is_some());
}

#[test]
fn item724_validated_attach_survives_title_changes_when_stamping_fails() {
    let conn = agentflare_backend::db::open_in_memory().unwrap();
    let (project, state) = test_project_with_in_review_state(&conn);
    let item = create_owner_item(&conn, &project, &state);
    let branch = format!("task/{}-old-slug", item.sequence_id);
    let mut pr = unstamped_pr(&branch, &item.name);
    let prs = serde_json::json!([{
        "number": 870, "html_url": "u", "state": "open", "title": item.name,
        "head": {"ref": branch}, "author_association": "OWNER"
    }])
    .to_string();
    let server = MockServer::start_with(move |req| {
        if req.method == "GET" && req.path.contains("/pulls?") {
            MockResponse::json(200, &prs)
        } else {
            MockResponse::json(500, "stamp unavailable")
        }
    });
    discover_untracked_prs(
        &conn,
        &server.client(Some("tok")),
        &RepoId {
            owner: "o".into(),
            repo: "r".into(),
        },
        &project,
        &state,
        &std::collections::HashSet::new(),
        "box-a",
        &|_| true,
    );
    let item = agentflare_backend::item::get(&conn, &item.id).unwrap();
    assert_eq!(super::super::pr_number_from_metadata(&item), Some(870));
    pr.title = "Edited title".into();
    assert!(super::super::pr_item_mismatch(&item, &pr).is_none());
}

#[test]
fn item724_explicit_adoption_validates_an_unstamped_legacy_mapping() {
    let conn = agentflare_backend::db::open_in_memory().unwrap();
    let (project, state) = test_project_with_in_review_state(&conn);
    let mut item = create_owner_item(&conn, &project, &state);
    let pr = unstamped_pr("fix/hand-opened", "Edited title");
    let mut meta =
        crate::mcp_server::metadata_object(r#"{"pr":{"number":870,"branch":"fix/hand-opened"}}"#);
    item.metadata = serde_json::to_string(&meta).unwrap();
    assert!(super::super::pr_item_mismatch(&item, &pr).is_some());
    super::super::record_adopted_identity(&mut meta, &pr);
    item.metadata = serde_json::to_string(&meta).unwrap();
    assert!(super::super::pr_item_mismatch(&item, &pr).is_none());
}

#[test]
fn item724_does_not_attach_an_unrelated_task_with_the_same_local_sequence() {
    let conn = agentflare_backend::db::open_in_memory().unwrap();
    let (project, state) = test_project_with_in_review_state(&conn);
    let item = create_owner_item(&conn, &project, &state);
    let branch = format!("task/{}-unrelated-auth-repair", item.sequence_id);
    let prs = serde_json::json!([{
        "number": 870, "html_url": "u", "state": "open",
        "title": "fix: unrelated authentication repair",
        "body": "Opened by agentflare for item #999 via agentflare.",
        "head": {"ref": branch, "sha": "abc"}, "author_association": "OWNER"
    }])
    .to_string();
    let comments = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let stored = comments.clone();
    let server =
        MockServer::start_with(move |req| match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/repos/o/r/pulls?state=open&per_page=100&page=1") => {
                MockResponse::json(200, &prs)
            }
            ("POST", "/repos/o/r/issues/870/labels") => MockResponse::json(200, "[]"),
            ("GET", "/repos/o/r/issues/870/comments?per_page=100&page=1") => MockResponse::json(
                200,
                &serde_json::to_string(&*stored.lock().unwrap()).unwrap(),
            ),
            ("POST", "/repos/o/r/issues/870/comments") => {
                let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
                stored.lock().unwrap().push(serde_json::json!({
                    "id": 1, "user": {"login": "agentflare"}, "body": body["body"]
                }));
                MockResponse::json(201, r#"{"id":1}"#)
            }
            _ => MockResponse::json(
                500,
                &format!("unexpected request: {} {}", req.method, req.path),
            ),
        });
    for _ in 0..2 {
        discover_untracked_prs(
            &conn,
            &server.client(Some("tok")),
            &RepoId {
                owner: "o".into(),
                repo: "r".into(),
            },
            &project,
            &state,
            &std::collections::HashSet::new(),
            "flared:box-a",
            &|_| true,
        );
    }
    let current = agentflare_backend::item::get(&conn, &item.id).unwrap();
    assert!(
        super::super::pr_number_from_metadata(&current).is_none(),
        "a matching local task number is not evidence of task identity"
    );
    let requests = server.requests();
    assert!(requests.iter().any(|r| r.method == "POST"
        && r.path.ends_with("/labels")
        && r.body.contains("agentflare:needs-human")));
    let comments = comments.lock().unwrap();
    assert_eq!(
        comments.len(),
        1,
        "a repeat sweep must not duplicate the reason"
    );
    assert!(
        comments[0]["body"]
            .as_str()
            .unwrap()
            .contains("Verify the PR-to-item mapping")
    );
}
