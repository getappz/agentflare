use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub html_url: String,
    pub state: String,
    pub title: String,
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
}
