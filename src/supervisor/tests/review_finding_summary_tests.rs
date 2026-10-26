use super::*;
use crate::supervisor::review_bots_tests::{BOT, comment, thread};

#[test]
fn finding_summary_uses_title_instead_of_coderabbit_html_header() {
    let header = r#"**<picture><img src="https://storage.googleapis.com/coderabbit_public_assets/review-comment-icons-v1/amber-16.svg" width="16" height="16" align="absmiddle" alt=""></picture> Minor** · **Functional Correctness** · **Quick win**"#;
    for (body, expected_title) in [
        (
            format!(
                "<!-- coderabbit-ai-generated:v1 -->\n\n{header}\n\n**Check the item-worktree fingerprint before the timestamp fallback.**"
            ),
            "Check the item-worktree fingerprint before the timestamp fallback.",
        ),
        (
            format!("{header}\n\nSkip worktree lookup for non-gated item actions."),
            "Skip worktree lookup for non-gated item actions.",
        ),
        (header.to_string(), ""),
    ] {
        let threads = vec![thread(
            "PRRT_html",
            false,
            vec![comment(1, BOT, &body, "t1")],
        )];
        let findings = classify_threads(&threads, &ReviewBotConfig::default(), &Default::default());
        let finding = &findings[0];
        assert_eq!(finding.parsed.title, expected_title);
        assert_eq!(finding.parsed.severity, Severity::Minor);
        let summary_title = if expected_title.is_empty() {
            "Review finding"
        } else {
            expected_title
        };
        assert_eq!(
            finding.summary_line(),
            format!("- `src/lib.rs:42` ({BOT}) [minor] round 1: {summary_title}")
        );
        assert_eq!(
            finding.body, body,
            "retain the original review for the repair task"
        );
    }
}
