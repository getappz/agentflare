//! Which agentflare instance owns a PR, and whether another may take it over
//! (item #347 phase 3). Every instance authenticates to GitHub as the same
//! user, so ownership lives in-band: a `takeover` marker comment, else the
//! origin stamp in the body, else the earliest discovery `claim` marker.

use crate::github::bridge::marker::{Action, Marker};
use crate::github::pulls::origin_of;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerSource {
    Takeover,
    Stamp,
    Claim,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub instance: String,
    pub source: OwnerSource,
}

/// Resolves the owning instance from a PR body and its `(comment id, body)`
/// pairs. The latest takeover wins (comment ids are monotonic), then the
/// origin stamp, then the earliest claim. `None` means nothing names an
/// owner (a hand-opened, never-claimed PR).
pub fn resolve_owner(body: Option<&str>, comments: &[(u64, String)]) -> Option<Owner> {
    let by_action = |action: Action| {
        comments
            .iter()
            .filter_map(|(id, text)| Some((*id, Marker::parse(text)?)))
            .filter(move |(_, m)| m.action == action)
    };
    if let Some((_, m)) = by_action(Action::Takeover).max_by_key(|(id, _)| *id) {
        return Some(Owner {
            instance: m.owner,
            source: OwnerSource::Takeover,
        });
    }
    if let Some(o) = origin_of(body) {
        return Some(Owner {
            instance: o.instance,
            source: OwnerSource::Stamp,
        });
    }
    by_action(Action::Claim)
        .min_by_key(|(id, _)| *id)
        .map(|(_, m)| Owner {
            instance: m.owner,
            source: OwnerSource::Claim,
        })
}

/// Whether `me` may take over a PR owned by `owner`. GitHub can't tell
/// instances apart, so liveness is the PR's `updated_at` (any commit,
/// comment or label bumps it): the owner counts as live inside `ttl_secs`.
/// An unparseable timestamp can't prove inactivity, so it refuses too.
pub fn adopt_check(
    owner: Option<&Owner>,
    me: &str,
    updated_at: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
    ttl_secs: i64,
    force: bool,
) -> Result<(), String> {
    let Some(owner) = owner else { return Ok(()) };
    if owner.instance == me {
        return Err("this instance already owns the PR".into());
    }
    if force {
        return Ok(());
    }
    let last = updated_at
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&chrono::Utc));
    match last {
        Some(t) if (now - t).num_seconds() >= ttl_secs => Ok(()),
        Some(t) => Err(format!(
            "owner {} was active at {t} (< {ttl_secs}s ago); wait for the TTL or pass --force",
            owner.instance
        )),
        None => Err(format!(
            "cannot tell when owner {} was last active; pass --force",
            owner.instance
        )),
    }
}

/// Open PRs whose origin stamp names an instance other than `me`, as
/// `(number, owning instance)`. Body stamp only, so it costs one list call.
pub fn foreign_stamped(prs: &[crate::github::models::PullRequest], me: &str) -> Vec<(u64, String)> {
    prs.iter()
        .filter_map(|pr| {
            let o = origin_of(pr.body.as_deref())?;
            (o.instance != me).then_some((pr.number, o.instance))
        })
        .collect()
}

/// Doctor line for PRs other instances own; `None` when GitHub isn't
/// reachable (no remote or credentials) or nothing is foreign.
pub fn doctor_line(repo_root: &std::path::Path) -> Option<String> {
    let repo = crate::github::RepoId::resolve_from_remote(repo_root)?;
    let client = crate::github::Client::new().ok()?;
    let prs = crate::github::pulls::list(&client, &repo, "open").ok()?;
    let foreign = foreign_stamped(&prs, &crate::github::bridge::config::stable_instance_id());
    if foreign.is_empty() {
        return None;
    }
    // Count only: the listed ids derive from the token-bearing client's data,
    // which CodeQL flags as a cleartext secret when printed. `pr owner <n>`
    // names the owner of a specific PR.
    Some(format!(
        "other-instance PRs: {} open (inspect with `agentflare pr owner <n>`)",
        foreign.len()
    ))
}

pub fn takeover_comment(me: &str, pr_number: u64) -> String {
    let marker = Marker {
        action: Action::Takeover,
        owner: me.to_string(),
        item: format!("pr-{pr_number}"),
        ts: chrono::Utc::now().timestamp(),
        hash: String::new(),
    };
    format!("Taken over by `{me}`.\n\n{}", marker.render())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::pulls::origin_tag;
    use chrono::TimeZone;

    fn marker_comment(id: u64, action: Action, owner: &str) -> (u64, String) {
        let m = Marker {
            action,
            owner: owner.into(),
            item: "x".into(),
            ts: 1,
            hash: String::new(),
        };
        (id, m.render())
    }

    #[test]
    fn takeover_beats_stamp_beats_claim() {
        let stamped = origin_tag("a", "u", 1, "b");
        let claim = marker_comment(1, Action::Claim, "c");
        assert_eq!(
            resolve_owner(None, std::slice::from_ref(&claim))
                .unwrap()
                .source,
            OwnerSource::Claim
        );
        let o = resolve_owner(Some(&stamped), std::slice::from_ref(&claim)).unwrap();
        assert_eq!((o.instance.as_str(), o.source), ("a", OwnerSource::Stamp));
        let comments = [
            claim,
            marker_comment(2, Action::Takeover, "t1"),
            marker_comment(3, Action::Takeover, "t2"),
        ];
        let o = resolve_owner(Some(&stamped), &comments).unwrap();
        assert_eq!(
            (o.instance.as_str(), o.source),
            ("t2", OwnerSource::Takeover)
        );
        assert!(resolve_owner(Some("hand made"), &[]).is_none());
    }

    #[test]
    fn adopt_requires_inactivity_or_force() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 10, 7, 12, 0, 0).unwrap();
        let owner = Owner {
            instance: "other".into(),
            source: OwnerSource::Stamp,
        };
        let ttl = 24 * 3600;
        let go = |o: Option<&Owner>, me, at, force| adopt_check(o, me, at, now, ttl, force);
        assert!(go(Some(&owner), "me", Some("2026-10-07T00:00:00Z"), false).is_err());
        assert!(go(Some(&owner), "me", Some("2026-10-05T00:00:00Z"), false).is_ok());
        assert!(go(Some(&owner), "me", Some("2026-10-07T00:00:00Z"), true).is_ok());
        assert!(go(Some(&owner), "me", None, false).is_err());
        assert!(go(Some(&owner), "other", None, true).is_err());
        assert!(go(None, "me", None, false).is_ok());
    }

    #[test]
    fn foreign_stamped_lists_only_other_instances() {
        let pr = |n: u64, body: String| -> crate::github::models::PullRequest {
            serde_json::from_value(serde_json::json!({
                "number": n, "html_url": "u", "state": "open", "title": "t", "body": body
            }))
            .unwrap()
        };
        let prs = [
            pr(1, origin_tag("me", "u", 1, "b")),
            pr(2, origin_tag("other", "u", 2, "b")),
            pr(3, "hand made".into()),
        ];
        assert_eq!(foreign_stamped(&prs, "me"), vec![(2, "other".to_string())]);
    }

    #[test]
    fn takeover_comment_round_trips() {
        let m = Marker::parse(&takeover_comment("me", 9)).unwrap();
        assert_eq!((m.action, m.owner.as_str()), (Action::Takeover, "me"));
    }
}
