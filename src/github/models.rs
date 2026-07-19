use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RefInfo {
    #[serde(rename = "ref")]
    pub git_ref: String,
    #[serde(default)]
    pub sha: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub html_url: String,
    pub state: String,
    pub title: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    #[allow(dead_code)]
    pub mergeable: Option<bool>,
    #[serde(default)]
    pub mergeable_state: Option<String>,
    #[serde(default)]
    pub additions: Option<u64>,
    #[serde(default)]
    pub deletions: Option<u64>,
    #[serde(default)]
    pub changed_files: Option<u64>,
    #[serde(default)]
    pub head: Option<RefInfo>,
    #[serde(default)]
    pub base: Option<RefInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    pub number: u64,
    pub html_url: String,
    pub state: String,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    #[allow(dead_code)]
    pub id: u64,
    pub tag_name: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub name: Option<String>,
    pub html_url: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowRun {
    pub id: u64,
    #[serde(default)]
    #[allow(dead_code)]
    pub name: Option<String>,
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    pub html_url: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub head_branch: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckRun {
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Review {
    pub user: User,
    pub state: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub submitted_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReviewComment {
    pub id: u64,
    pub user: User,
    pub path: String,
    #[serde(default)]
    pub line: Option<u64>,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Comment {
    pub user: User,
    pub body: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub created_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_request_deserializes_from_rest_payload() {
        let json = serde_json::json!({
            "number": 7, "html_url": "https://github.com/o/r/pull/7",
            "state": "open", "title": "Add thing", "extra_ignored_field": true
        });
        let pr: PullRequest = serde_json::from_value(json).unwrap();
        assert_eq!(pr.number, 7);
        assert_eq!(pr.html_url, "https://github.com/o/r/pull/7");
        assert_eq!(pr.state, "open");
    }

    #[test]
    fn issue_deserializes_and_ignores_extra_fields() {
        let json = serde_json::json!({
            "number": 42, "html_url": "https://github.com/o/r/issues/42",
            "state": "open", "title": "Bug", "labels": [], "pull_request": null
        });
        let issue: Issue = serde_json::from_value(json).unwrap();
        assert_eq!(issue.number, 42);
        assert_eq!(issue.state, "open");
        assert_eq!(issue.title, "Bug");
    }
}

#[cfg(test)]
mod release_tests {
    use super::*;

    #[test]
    fn release_deserializes_with_defaults() {
        let json = serde_json::json!({
            "id": 900, "tag_name": "v1.2.3",
            "html_url": "https://github.com/o/r/releases/tag/v1.2.3"
        });
        let rel: Release = serde_json::from_value(json).unwrap();
        assert_eq!(rel.tag_name, "v1.2.3");
        assert_eq!(rel.name, None);
        assert!(!rel.draft);
        assert!(!rel.prerelease);
    }
}

#[cfg(test)]
mod workflow_run_tests {
    use super::*;
    #[test]
    fn workflow_run_deserializes_with_optional_conclusion() {
        let json = serde_json::json!({
            "id": 555, "name": "CI", "status": "in_progress", "conclusion": null,
            "html_url": "https://github.com/o/r/actions/runs/555", "head_branch": "feat/x"
        });
        let run: WorkflowRun = serde_json::from_value(json).unwrap();
        assert_eq!(run.id, 555);
        assert_eq!(run.status, "in_progress");
        assert_eq!(run.conclusion, None);
        assert_eq!(run.head_branch.as_deref(), Some("feat/x"));
    }
}

#[cfg(test)]
mod pr_status_model_tests {
    use super::*;

    #[test]
    fn pull_request_deserializes_review_fields() {
        let json = serde_json::json!({
            "number": 5, "html_url": "u", "state": "open", "title": "t",
            "draft": true, "mergeable": false, "mergeable_state": "dirty",
            "additions": 10, "deletions": 2, "changed_files": 3,
            "head": {"ref": "feat/x", "sha": "abc123"},
            "base": {"ref": "main", "sha": "def456"}
        });
        let pr: PullRequest = serde_json::from_value(json).unwrap();
        assert!(pr.draft);
        assert_eq!(pr.mergeable, Some(false));
        assert_eq!(pr.mergeable_state.as_deref(), Some("dirty"));
        assert_eq!(pr.additions, Some(10));
        assert_eq!(pr.head.unwrap().sha, "abc123");
        assert_eq!(pr.base.unwrap().git_ref, "main");
    }

    #[test]
    fn check_run_deserializes_with_null_conclusion() {
        let json =
            serde_json::json!({ "name": "build", "status": "in_progress", "conclusion": null });
        let run: CheckRun = serde_json::from_value(json).unwrap();
        assert_eq!(run.name, "build");
        assert_eq!(run.conclusion, None);
    }

    #[test]
    fn review_deserializes_author_and_state() {
        let json = serde_json::json!({
            "user": {"login": "coderabbitai[bot]"}, "state": "CHANGES_REQUESTED",
            "body": "fix this", "submitted_at": "2026-07-19T00:00:00Z"
        });
        let review: Review = serde_json::from_value(json).unwrap();
        assert_eq!(review.user.login, "coderabbitai[bot]");
        assert_eq!(review.state, "CHANGES_REQUESTED");
    }

    #[test]
    fn review_comment_line_is_optional() {
        let json = serde_json::json!({
            "id": 900, "user": {"login": "alice"}, "path": "src/x.rs", "line": null, "body": "nit"
        });
        let c: ReviewComment = serde_json::from_value(json).unwrap();
        assert_eq!(c.line, None);
        assert_eq!(c.path, "src/x.rs");
    }
}
