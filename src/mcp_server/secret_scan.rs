//! Pre-handoff secret scan — fail-closed guard so a pasted credential in a
//! `handoff` payload never becomes a stored asset/item. `handoff_impl`
//! persists `content` (+ facts/summary/findings/decisions) straight to
//! `blob_store`/`doc_upsert_with_opts` with no check of its own;
//! `gateway_registry::redact_error_for_llm` only ever runs on *error*
//! strings, not on payloads a caller supplies on purpose.
//!
//! High-confidence patterns only — this must not block legitimate handoffs
//! on prose that merely mentions "token" or "sk-something". Each pattern
//! below requires the specific prefix/shape real credentials have (e.g.
//! `sk-live-`/`sk-test-`, not bare `sk-`).

use regex::Regex;
use rmcp::model::ErrorData;
use std::sync::LazyLock;

struct SecretPattern {
    name: &'static str,
    re: Regex,
}

static PATTERNS: LazyLock<Vec<SecretPattern>> = LazyLock::new(|| {
    vec![
        SecretPattern {
            name: "GitHub token",
            re: Regex::new(r"gh[po]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}").unwrap(),
        },
        SecretPattern {
            name: "live/test secret key",
            re: Regex::new(r"sk-(live|test)-[A-Za-z0-9]{10,}").unwrap(),
        },
        SecretPattern {
            name: "AWS access key id",
            re: Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
        },
        SecretPattern {
            name: "Slack token",
            re: Regex::new(r"xox[bpas]-[A-Za-z0-9-]+").unwrap(),
        },
        SecretPattern {
            name: "private key block",
            re: Regex::new(r"-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----").unwrap(),
        },
        SecretPattern {
            name: "bearer token",
            re: Regex::new(r"Bearer\s+[A-Za-z0-9\-._~+/]+").unwrap(),
        },
        SecretPattern {
            name: "AWS secret access key",
            re: Regex::new(r#"(?i)aws_secret_access_key\s*[:=]\s*['"]?[A-Za-z0-9/+=]{20,}"#)
                .unwrap(),
        },
    ]
});

fn find_secret(text: &str) -> Option<&'static str> {
    PATTERNS
        .iter()
        .find(|p| p.re.is_match(text))
        .map(|p| p.name)
}

/// Checks each `(field, text)` pair and fails on the first match — naming
/// the field and pattern class in the error, never the matched text itself,
/// so the secret doesn't round-trip back through the LLM that triggered
/// this rejection.
pub(crate) fn check_fields<'a>(
    fields: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<(), ErrorData> {
    for (field, text) in fields {
        if let Some(pattern) = find_secret(text) {
            return Err(ErrorData::invalid_params(
                format!(
                    "handoff blocked: field '{field}' looks like it contains a {pattern} -- \
                     refusing to persist a credential to a stored asset/item. Remove it, or \
                     pass allow_secrets=true if this is an intentional credential handoff."
                ),
                None,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_github_token() {
        assert_eq!(
            find_secret("token: ghp_abcdefghijklmnopqrstuvwxyz012345"),
            Some("GitHub token")
        );
    }

    #[test]
    fn detects_github_fine_grained_pat() {
        assert_eq!(
            find_secret("github_pat_11ABCDEFG0abcdefghijklmnopqrstuvwxyz"),
            Some("GitHub token")
        );
    }

    #[test]
    fn detects_live_secret_key() {
        assert_eq!(
            find_secret("use sk-live-51H8xyzABCDEFghij in prod"),
            Some("live/test secret key")
        );
    }

    #[test]
    fn does_not_flag_sk_prefix_in_prose() {
        // "sk-" without a live/test suffix and enough entropy is common in
        // prose (e.g. abbreviations) and must not trip the scanner.
        assert_eq!(find_secret("the sk-8 rocket variant"), None);
        assert_eq!(find_secret("see doc section sk-overview"), None);
    }

    #[test]
    fn detects_aws_access_key() {
        assert_eq!(
            find_secret("AKIAIOSFODNN7EXAMPLE"),
            Some("AWS access key id")
        );
    }

    #[test]
    fn detects_slack_token() {
        assert_eq!(
            find_secret("xoxb-1234567890-abcdefghij"),
            Some("Slack token")
        );
    }

    #[test]
    fn detects_private_key_header() {
        assert_eq!(
            find_secret("-----BEGIN RSA PRIVATE KEY-----\nMIIB..."),
            Some("private key block")
        );
    }

    #[test]
    fn detects_bearer_token() {
        assert_eq!(
            find_secret("Authorization: Bearer abc123.def456-ghi"),
            Some("bearer token")
        );
    }

    #[test]
    fn detects_aws_secret_access_key_pair() {
        assert_eq!(
            find_secret("aws_secret_access_key=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"),
            Some("AWS secret access key")
        );
    }

    #[test]
    fn clean_prose_passes() {
        assert_eq!(
            find_secret("finished the refactor, tests pass, no blockers"),
            None
        );
    }

    #[test]
    fn check_fields_names_field_and_pattern_without_echoing_secret() {
        let err =
            check_fields([("content", "leaked ghp_abcdefghijklmnopqrstuvwxyz012345")]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("content"), "{msg}");
        assert!(msg.contains("GitHub token"), "{msg}");
        assert!(
            !msg.contains("ghp_abcdefghijklmnopqrstuvwxyz012345"),
            "{msg}"
        );
    }

    #[test]
    fn check_fields_passes_clean_input() {
        assert!(check_fields([("content", "all good here")]).is_ok());
    }
}
