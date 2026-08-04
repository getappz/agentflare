//! The manual verification checklist, as a test you can actually re-run.
//!
//! Everything else in the bridge's suite talks to a mock. That leaves one
//! class of question no mock can answer, because the answer lives in
//! GitHub's own behavior rather than in our code:
//!
//! - does the marker really render INVISIBLY in the web UI? Every other test
//!   asserts on the marker's *text*. If GitHub ever stopped hiding HTML
//!   comments inside issue-comment bodies, the whole protocol would become
//!   visible noise on every issue and not one mock test would notice.
//! - does editing a comment really leave its id (and therefore the claim
//!   ordering `resolve_holder` depends on) untouched?
//! - are `claimed:` labels with colons in them really addressable on the
//!   label-delete endpoint?
//!
//! IGNORED by default and gated on an env var, so it can never run in CI or
//! surprise anyone: it posts real comments and moves real labels.
//!
//! ```text
//! gh repo create my-scratch --private
//! gh label create agentflare --repo OWNER/my-scratch
//! gh issue create --repo OWNER/my-scratch --title t --label agentflare
//!
//! AGENTFLARE_LIVE_GITHUB_REPO=OWNER/my-scratch \
//!   cargo test --bin agentflare live_github -- --ignored --nocapture
//! ```
//!
//! It deliberately leaves its comments and labels behind: they are the
//! evidence you eyeball in the browser afterwards.

use crate::github::bridge::config::BridgeConfig;
use crate::github::bridge::items;
use crate::github::bridge::marker::{Action, Marker};
use crate::github::bridge::tick::{Ctx, run_once};
use crate::github::{Client, RepoId, issues};

/// `run_once` takes `now` as a parameter, so the whole TTL schedule can be
/// driven in seconds instead of waiting out a real half-hour.
const TTL: i64 = 1800;

struct Live {
    ctx: Ctx,
    conn: rusqlite::Connection,
    project_id: String,
    instance: String,
}

fn live_repo() -> Option<RepoId> {
    let spec = std::env::var("AGENTFLARE_LIVE_GITHUB_REPO").ok()?;
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    Some(RepoId::parse(spec).expect("AGENTFLARE_LIVE_GITHUB_REPO must be owner/repo"))
}

fn live(repo: RepoId, max_claims: usize) -> Live {
    // A fresh owner id per run, so re-running against the same scratch repo
    // never collides with the markers a previous run left behind.
    let instance = {
        use rand::Rng;
        let n: [u8; 4] = rand::thread_rng().r#gen();
        format!("live-verify:{}", hex::encode(n))
    };
    let (conn, project_id) = crate::github::bridge::items::tests::tests_support_db();
    let ledger = rusqlite::Connection::open_in_memory().unwrap();
    crate::claims::migrate(&ledger).unwrap();
    Live {
        ctx: Ctx {
            client: Client::new().expect("needs a GitHub credential — try `gh auth login`"),
            repo,
            config: BridgeConfig::from_values(
                Some("1"),
                None,
                Some(&max_claims.to_string()),
                None,
                instance.clone(),
            ),
            project_id: project_id.clone(),
            ledger,
        },
        conn,
        project_id,
        instance,
    }
}

/// Puts the scratch repo back to "nothing has ever run here": every
/// agentflare marker comment deleted, every `claimed:` label removed, every
/// closed queue issue reopened.
///
/// Needed because each run leaves live claims behind, and a claim stays live
/// for a whole TTL — without a reset the second run finds the entire queue
/// held and can never claim anything. It is also what makes the leftovers
/// safe to eyeball: you always see exactly one run's worth.
///
/// DESTRUCTIVE, which is why this whole file is `#[ignore]`d and gated on an
/// env var naming a throwaway repo. It only ever deletes comments that parse
/// as agentflare markers — a human comment on a scratch issue survives.
fn reset_scratch_repo(ctx: &Ctx) {
    let queued = issues::list_filtered(
        &ctx.client,
        &ctx.repo,
        "all",
        Some(&ctx.config.queue_label),
        None,
    )
    .expect("list the queue");

    for issue in &queued {
        for c in issues::list_comments(&ctx.client, &ctx.repo, issue.number, None)
            .expect("list comments")
        {
            if Marker::parse(&c.body).is_some() {
                issues::delete_comment(&ctx.client, &ctx.repo, c.id)
                    .expect("delete marker comment");
            }
        }
        for label in issue.labels.iter().map(|l| l.name.clone()) {
            if label.starts_with("claimed:") {
                issues::remove_label(&ctx.client, &ctx.repo, issue.number, &label)
                    .expect("remove claimed label");
            }
        }
        if issue.state == "closed" {
            issues::reopen(&ctx.client, &ctx.repo, issue.number).expect("reopen");
        }
    }
    println!("reset {} scratch issue(s) to a clean slate", queued.len());
}

impl Live {
    fn tick(&self, now: i64) -> crate::github::bridge::tick::TickReport {
        run_once(&self.ctx, &self.conn, now).expect("live tick must not hard-error")
    }

    /// Every comment on `number` carrying one of OUR markers.
    fn our_markers(&self, number: u64) -> Vec<(u64, Marker)> {
        issues::list_comments(&self.ctx.client, &self.ctx.repo, number, None)
            .expect("list comments")
            .into_iter()
            .filter_map(|c| Marker::parse(&c.body).map(|m| (c.id, m)))
            .filter(|(_, m)| m.owner == self.instance)
            .collect()
    }

    fn labels(&self, number: u64) -> Vec<String> {
        issues::get(&self.ctx.client, &self.ctx.repo, number)
            .expect("get issue")
            .labels
            .into_iter()
            .map(|l| l.name)
            .collect()
    }
}

/// The comment as GitHub RENDERS it. This is the whole reason the test exists:
/// `body` is what we wrote, `body_html` is what a human sees.
fn rendered_html(ctx: &Ctx, comment_id: u64) -> String {
    let path = format!(
        "/repos/{}/{}/issues/comments/{comment_id}",
        ctx.repo.owner, ctx.repo.repo
    );
    let json = ctx
        .client
        .request_with_accept("GET", &path, None, "application/vnd.github.html+json")
        .expect("fetch rendered comment");
    json["body_html"].as_str().unwrap_or_default().to_string()
}

/// Reset only — no assertions. Handy between manual daemon runs, which leave
/// live claims behind just like the tests do.
///
/// `cargo test --bin agentflare live_github_reset -- --ignored --nocapture`
#[test]
#[ignore = "destructive: wipes agentflare markers from a real repo — set AGENTFLARE_LIVE_GITHUB_REPO"]
fn live_github_reset() {
    let Some(repo) = live_repo() else {
        panic!("set AGENTFLARE_LIVE_GITHUB_REPO=owner/repo (see this file's docs)");
    };
    reset_scratch_repo(&live(repo, 0).ctx);
}

#[test]
#[ignore = "posts to a real GitHub repo — set AGENTFLARE_LIVE_GITHUB_REPO and run with --ignored"]
fn live_github_bridge_lifecycle() {
    let Some(repo) = live_repo() else {
        panic!("set AGENTFLARE_LIVE_GITHUB_REPO=owner/repo (see this file's docs)");
    };
    let l = live(repo, 1);
    reset_scratch_repo(&l.ctx);
    let t0 = crate::claims::now();

    // --- claim -------------------------------------------------------------
    let claimed = l.tick(t0);
    assert_eq!(
        claimed.claimed.len(),
        1,
        "expected exactly one claim, got {claimed:?} — does the repo have an \
         open issue labelled `agentflare` that nobody holds?"
    );
    let number = claimed.claimed[0];
    println!("claimed #{number} as {}", l.instance);

    let markers = l.our_markers(number);
    assert_eq!(markers.len(), 1, "exactly one claim comment");
    let (comment_id, claim_marker) = markers[0].clone();
    assert_eq!(claim_marker.action, Action::Claim);

    let item = items::find_by_issue(&l.conn, &l.project_id, number).expect("local item");
    assert_eq!(
        claim_marker.item, item.id,
        "the marker must name the real item, not `pending` (M2)"
    );

    // --- the marker must be INVISIBLE -------------------------------------
    //
    // Note what is NOT asserted: the instance id itself. The bridge writes a
    // deliberate human-readable sentence ("Claiming this for `x`.") and that
    // SHOULD render — the point is that a person can tell who took the issue.
    // What must never render is the machine part: the version tag and the
    // `key=value` fields. Checking for the owner id instead would fail on the
    // prose we want.
    let raw = issues::list_comments(&l.ctx.client, &l.ctx.repo, number, None)
        .expect("list comments")
        .into_iter()
        .find(|c| c.id == comment_id)
        .expect("our comment")
        .body;
    assert!(
        raw.contains(crate::github::bridge::marker::MARKER_VERSION),
        "sanity: the comment we are about to check really does carry a marker"
    );

    let html = rendered_html(&l.ctx, comment_id);
    println!("rendered html: {html}");
    for leaked in [
        crate::github::bridge::marker::MARKER_VERSION,
        "action=",
        "ts=",
        "hash=",
        "<!--",
    ] {
        assert!(
            !html.contains(leaked),
            "{leaked:?} leaked into the RENDERED comment — GitHub is no longer \
             hiding the marker, so every issue the bridge touches now carries \
             visible machine noise, and no mock test would ever catch it. \
             Rendered: {html}"
        );
    }

    assert!(
        l.labels(number)
            .contains(&format!("claimed:{}", l.instance)),
        "the claimed: label must be applied"
    );

    // --- a fresh marker is left alone -------------------------------------
    l.tick(t0 + 10);
    let after_quiet = l.our_markers(number);
    assert_eq!(
        after_quiet.len(),
        1,
        "no new comment while the marker is fresh"
    );
    assert_eq!(
        after_quiet[0].1.ts, claim_marker.ts,
        "and no pointless rewrite either"
    );

    // --- heartbeat EDITS rather than appends -------------------------------
    let hb_at = t0 + TTL / 2 + 1;
    l.tick(hb_at);
    let after_hb = l.our_markers(number);
    assert_eq!(
        after_hb.len(),
        1,
        "the heartbeat must EDIT the claim comment, not post another one — \
         got {} marker comments",
        after_hb.len()
    );
    assert_eq!(
        after_hb[0].0, comment_id,
        "and must keep the SAME comment id: resolve_holder orders claims by \
         it, so a new id would silently reorder the race"
    );
    assert!(
        after_hb[0].1.ts > claim_marker.ts,
        "the refreshed marker must carry a newer ts ({} vs {})",
        after_hb[0].1.ts,
        claim_marker.ts
    );
    println!(
        "heartbeat edited comment {comment_id} in place, ts {} -> {}",
        claim_marker.ts, after_hb[0].1.ts
    );

    // --- surviving well past the TTL is the C1 regression ------------------
    let mut t = hb_at;
    while t < t0 + TTL * 3 {
        t += 60;
        l.tick(t);
        assert!(
            items::find_by_issue(&l.conn, &l.project_id, number)
                .is_some_and(|i| items::is_active(&l.conn, &i)),
            "the instance ceded its own live work at t0 + {} (C1)",
            t - t0
        );
    }
    let sustained = l.our_markers(number);
    assert!(
        !sustained.iter().any(|(_, m)| m.action == Action::Cede),
        "an instance must never cede work it is still doing"
    );
    assert_eq!(
        sustained.len(),
        1,
        "and must still be using one comment after 3 TTLs of ticks, not {}",
        sustained.len()
    );
    println!("held #{number} across 3 TTLs using exactly 1 comment");

    // --- done closes the issue and drops the label -------------------------
    let done_state = items::state_id_for_group(&l.conn, &l.project_id, "completed").unwrap();
    agentflare_backend::item::update_state(&l.conn, &item.id, &done_state).unwrap();
    l.tick(t + 60);

    let finished = l.our_markers(number);
    assert!(
        finished.iter().any(|(_, m)| m.action == Action::Done),
        "completing the item must post a done marker"
    );
    assert_eq!(
        issues::get(&l.ctx.client, &l.ctx.repo, number)
            .unwrap()
            .state,
        "closed",
        "and close the issue"
    );
    assert!(
        !l.labels(number)
            .contains(&format!("claimed:{}", l.instance)),
        "and take the claimed: label back off (M3) — a colon in the label name \
         has to survive path encoding for this to work"
    );
    println!("completed #{number}: done marker posted, issue closed, label removed");
}

#[test]
#[ignore = "posts to a real GitHub repo — set AGENTFLARE_LIVE_GITHUB_REPO and run with --ignored"]
fn live_github_cede_marker_is_readable_by_another_instance() {
    // The bug the mock suite could not have caught until two real instances
    // shared one issue: `cede` wrote `hash=` (empty), and `Marker::parse`
    // fails closed on an empty value, so every cede was unparseable. Proven
    // here end to end — `b` must be able to READ `a`'s cede off real GitHub.
    let Some(repo) = live_repo() else {
        panic!("set AGENTFLARE_LIVE_GITHUB_REPO=owner/repo (see this file's docs)");
    };
    let a = live(repo.clone(), 1);
    let b = live(repo, 1);
    reset_scratch_repo(&a.ctx);
    let t0 = crate::claims::now();

    let claimed = a.tick(t0);
    assert_eq!(
        claimed.claimed.len(),
        1,
        "a must claim something: {claimed:?}"
    );
    let number = claimed.claimed[0];

    // `a` goes quiet past the TTL; `b` takes over; `a` comes back and cedes.
    let after_ttl = t0 + TTL + 1;
    let b_claimed = b.tick(after_ttl);
    assert!(
        b_claimed.claimed.contains(&number),
        "b should have taken over #{number} once a's marker expired: {b_claimed:?}"
    );
    let ceded = a.tick(after_ttl + 1);
    assert!(
        ceded.ceded.contains(&number),
        "a must cede #{number}: {ceded:?}"
    );

    let cede = a
        .our_markers(number)
        .into_iter()
        .find(|(_, m)| m.action == Action::Cede);
    assert!(
        cede.is_some(),
        "a's cede marker must be PARSEABLE off real GitHub — an unreadable \
         cede is invisible to every other instance"
    );
    assert!(
        !a.labels(number)
            .contains(&format!("claimed:{}", a.instance)),
        "and a must have dropped its claimed: label"
    );
    println!("#{number}: a ceded to b, and the cede marker reads back cleanly");
}
