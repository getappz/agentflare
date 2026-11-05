use super::*;
use crate::review::diff::parse_unified_diff;
use crate::supervisor::tests::coderabbit_finding;

#[test]
fn fix_task_diff_cache_pins_review_commit_and_falls_back_to_head() {
    let client = Client::anonymous();
    let repo = RepoId {
        owner: "test".into(),
        repo: "repo".into(),
    };
    let cache = FixTaskDiffCache::new(
        &client,
        &repo,
        1,
        "head",
        HashMap::from([(1, "review".into()), (2, String::new())]),
    );
    // Preseed both entries: any attempted network lookup would be a test bug.
    for (commit, code) in [("review", "review code"), ("head", "head code")] {
        cache.files_by_commit.borrow_mut().insert(
            commit.into(),
            parse_unified_diff(&format!(
                "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -42 +42 @@\n-old\n+{code}\n"
            )),
        );
    }
    for id in 1..=3 {
        let finding = coderabbit_finding(id, "coderabbitai[bot]");
        let FixTaskDiff::Window(window) = cache.diff_for(&finding) else {
            panic!("expected cached context");
        };
        assert!(window.contains(if id == 1 { "review code" } else { "head code" }));
        assert!(!window.contains(if id == 1 { "head code" } else { "review code" }));
    }
}
