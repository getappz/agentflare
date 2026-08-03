//! The hidden HTML-comment footer the bridge writes on every issue and
//! comment it authors. Both instances authenticate to GitHub as the same
//! user, so the GitHub actor cannot tell them apart — or tell an agent from
//! a human. This marker carries that discriminator in-band, which also means
//! an instance that loses its database can rebuild its view from GitHub.
//!
//! Parsing FAILS CLOSED: anything unparseable is `None`, i.e. treated as
//! human-authored. The poll loop must never halt on a malformed body.

use sha2::{Digest, Sha256};

pub const MARKER_VERSION: &str = "agentflare:v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Claim,
    Progress,
    Done,
    Cede,
}

impl Action {
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::Claim => "claim",
            Action::Progress => "progress",
            Action::Done => "done",
            Action::Cede => "cede",
        }
    }

    pub fn parse(s: &str) -> Option<Action> {
        match s {
            "claim" => Some(Action::Claim),
            "progress" => Some(Action::Progress),
            "done" => Some(Action::Done),
            "cede" => Some(Action::Cede),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub action: Action,
    pub owner: String,
    pub item: String,
    pub ts: i64,
    pub hash: String,
}

/// Stands in for a field with nothing to say — a `cede` has no content hash.
///
/// `parse` fails closed on an empty value, so a field rendered as `hash=`
/// makes the WHOLE marker unparseable. That is not hypothetical: `cede` wrote
/// exactly that, so every cede the bridge posted was invisible to the parser
/// and no other instance could see a claim being given up. The substitution
/// lives in `render` rather than at the call sites so a future field with an
/// empty value cannot reintroduce it.
const EMPTY_FIELD: &str = "-";

fn field(value: &str) -> &str {
    if value.is_empty() { EMPTY_FIELD } else { value }
}

impl Marker {
    pub fn render(&self) -> String {
        format!(
            "<!-- {} action={} owner={} item={} ts={} hash={} -->",
            MARKER_VERSION,
            self.action.as_str(),
            field(&self.owner),
            field(&self.item),
            self.ts,
            field(&self.hash)
        )
    }

    /// Parses the LAST marker in `text`, so a fresh footer appended to a body
    /// that already carried one wins.
    pub fn parse(text: &str) -> Option<Marker> {
        let open = format!("<!-- {MARKER_VERSION} ");
        let start = text.rfind(&open)?;
        let rest = &text[start + open.len()..];
        let end = rest.find(" -->")?;
        let fields = &rest[..end];
        // A nested comment delimiter means the input is malformed or crafted.
        if fields.contains("<!--") || fields.contains("-->") {
            return None;
        }

        let (mut action, mut owner, mut item, mut ts, mut hash) = (None, None, None, None, None);
        for pair in fields.split_whitespace() {
            // Any field without `=` means we are not looking at a well-formed
            // marker — bail rather than guess.
            let (k, v) = pair.split_once('=')?;
            if v.is_empty() {
                return None;
            }
            match k {
                "action" => action = Action::parse(v),
                "owner" => owner = Some(v.to_string()),
                "item" => item = Some(v.to_string()),
                "ts" => ts = v.parse::<i64>().ok(),
                "hash" => hash = Some(v.to_string()),
                _ => {}
            }
        }
        Some(Marker {
            action: action?,
            owner: owner?,
            item: item?,
            ts: ts?,
            hash: hash?,
        })
    }
}

/// Short digest of the semantically-meaningful content of an issue. Used both
/// as the marker's `hash` and as the export gate (`github_last_hash`), so it
/// MUST be stable across machines and releases — hence sha2 rather than
/// `DefaultHasher`, whose output is explicitly not guaranteed stable.
///
/// Labels are sorted so ordering churn from the GitHub API is not mistaken
/// for a real change, and fields are NUL-separated so ("ab","c") and
/// ("a","bc") cannot collide.
pub fn content_hash(title: &str, body: &str, state: &str, labels: &[String]) -> String {
    let mut sorted: Vec<&str> = labels.iter().map(String::as_str).collect();
    sorted.sort_unstable();

    let mut hasher = Sha256::new();
    for field in [title, body, state] {
        hasher.update(field.as_bytes());
        hasher.update([0u8]);
    }
    for label in sorted {
        hasher.update(label.as_bytes());
        hasher.update([0u8]);
    }
    hex::encode(&hasher.finalize()[..8])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Marker {
        Marker {
            action: Action::Claim,
            owner: "claude-code:desk-a".to_string(),
            item: "3f2ab91".to_string(),
            ts: 1_754_000_000,
            hash: "9c7e1a2".to_string(),
        }
    }

    #[test]
    fn render_then_parse_round_trips() {
        let m = sample();
        assert_eq!(Marker::parse(&m.render()), Some(m));
    }

    #[test]
    fn anything_render_writes_is_something_parse_can_read_back() {
        // The bug this guards: `cede` built its marker with `hash: ""`, which
        // rendered as `hash=` — and `parse` fails closed on an empty value,
        // so the entire marker was unparseable. Every cede the bridge ever
        // posted was invisible to the parser, meaning no other instance could
        // see a claim being given up. The unit tests all missed it because
        // their fixtures used a non-empty hash the production code never
        // wrote. Nothing `render` emits may be unreadable, whatever the input.
        for empty in ["owner", "item", "hash"] {
            let mut m = sample();
            match empty {
                "owner" => m.owner = String::new(),
                "item" => m.item = String::new(),
                _ => m.hash = String::new(),
            }
            let rendered = m.render();
            let parsed = Marker::parse(&rendered)
                .unwrap_or_else(|| panic!("empty {empty} made the marker unreadable: {rendered}"));
            assert_eq!(parsed.action, m.action);
            assert_eq!(parsed.ts, m.ts);
        }
    }

    #[test]
    fn a_cede_marker_as_the_bridge_actually_builds_it_round_trips() {
        // `tick::cede` has no content hash to report, so it passes "".
        let cede = Marker {
            action: Action::Cede,
            owner: "a:1".to_string(),
            item: "item-id".to_string(),
            ts: 1_754_000_000,
            hash: String::new(),
        };
        let parsed = Marker::parse(&cede.render()).expect("a cede must be readable");
        assert_eq!(parsed.action, Action::Cede);
        assert_eq!(parsed.owner, "a:1");
    }

    #[test]
    fn parse_finds_marker_after_human_text() {
        let body = format!("Please look at this.\n\n{}", sample().render());
        assert_eq!(Marker::parse(&body).unwrap().owner, "claude-code:desk-a");
    }

    #[test]
    fn parse_takes_the_last_marker_when_several_are_present() {
        let mut later = sample();
        later.ts = 1_754_000_999;
        let body = format!("{}\n{}", sample().render(), later.render());
        assert_eq!(Marker::parse(&body).unwrap().ts, 1_754_000_999);
    }

    #[test]
    fn malformed_input_parses_as_absent_never_errors() {
        for bad in [
            "",
            "no marker here",
            "<!-- agentflare:v1 -->",
            "<!-- agentflare:v1 action=claim -->",
            "<!-- agentflare:v2 action=claim owner=a item=b ts=1 hash=h -->",
            "<!-- agentflare:v1 action=bogus owner=a item=b ts=1 hash=h -->",
            "<!-- agentflare:v1 action=claim owner=a item=b ts=notanumber hash=h -->",
            "<!-- agentflare:v1 action=claim owner= item=b ts=1 hash=h -->",
            "<!-- agentflare:v1 actionclaim owner=a item=b ts=1 hash=h -->",
            "<!-- agentflare:v1 action=claim owner=a item=b ts=1 hash=h",
            "héllo <!-- agentflare:v1 broken",
        ] {
            assert_eq!(Marker::parse(bad), None, "should not parse: {bad:?}");
        }
    }

    #[test]
    fn every_action_round_trips() {
        for a in [Action::Claim, Action::Progress, Action::Done, Action::Cede] {
            let mut m = sample();
            m.action = a.clone();
            assert_eq!(Marker::parse(&m.render()).unwrap().action, a);
        }
    }

    #[test]
    fn content_hash_is_stable_and_label_order_independent() {
        let a = content_hash("t", "b", "open", &["x".into(), "y".into()]);
        let b = content_hash("t", "b", "open", &["y".into(), "x".into()]);
        assert_eq!(a, b, "label order must not change the hash");
        assert_eq!(a, content_hash("t", "b", "open", &["x".into(), "y".into()]));
    }

    #[test]
    fn content_hash_changes_when_any_field_changes() {
        let base = content_hash("t", "b", "open", &["x".into()]);
        assert_ne!(base, content_hash("T", "b", "open", &["x".into()]));
        assert_ne!(base, content_hash("t", "B", "open", &["x".into()]));
        assert_ne!(base, content_hash("t", "b", "closed", &["x".into()]));
        assert_ne!(base, content_hash("t", "b", "open", &["z".into()]));
    }

    #[test]
    fn content_hash_is_not_confused_by_field_boundaries() {
        // Without separators, ("ab","c") and ("a","bc") would collide.
        assert_ne!(
            content_hash("ab", "c", "open", &[]),
            content_hash("a", "bc", "open", &[])
        );
    }
}
