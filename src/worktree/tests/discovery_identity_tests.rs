use super::tests::{create_owner_item, test_project_with_in_review_state};
use super::*;
use crate::github::{
    RepoId,
    test_support::{MockResponse, MockServer},
};

#[test]
fn item724_does_not_attach_an_unrelated_task_with_the_same_local_sequence() {
    let conn = agentflare_backend::db::open_in_memory().unwrap();
    let (project, state) = test_project_with_in_review_state(&conn);
    let item = create_owner_item(&conn, &project, &state);
    let branch = format!("task/{}-unrelated-auth-repair", item.sequence_id);
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            &serde_json::json!([{
                "number": 870, "html_url": "u", "state": "open",
                "title": "fix: unrelated authentication repair",
                "body": "Opened by agentflare for item #999 via agentflare.",
                "head": {"ref": branch, "sha": "abc"}, "author_association": "OWNER"
            }])
            .to_string(),
        ),
        MockResponse::json(200, "{}"),
    ]);
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
    let current = agentflare_backend::item::get(&conn, &item.id).unwrap();
    assert!(
        super::super::pr_number_from_metadata(&current).is_none(),
        "a matching local task number is not evidence of task identity"
    );
}
