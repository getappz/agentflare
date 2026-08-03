//! Two instances, two private databases, two private ledgers, one shared
//! GitHub issue — driven through the real `run_once`.
//!
//! This file used to call `i_hold`/`resolve_holder` on hand-built comment
//! vectors. That proved nothing: `resolve_holder` returns `Option<Holder>`,
//! so "exactly one owner" is a property of the return type, not of the
//! protocol, and every one of those assertions passed while the tick
//! orchestration was ceding its own live work every TTL. The bugs were never
//! in the pure arbitration functions; they were in what `tick.rs` did with
//! the answers. So this drives `run_once` end to end and asserts on the two
//! private databases and the shared issue, which is where the bugs lived.

use crate::github::bridge::config::BridgeConfig;
use crate::github::bridge::items;
use crate::github::bridge::marker::{Action, Marker};
use crate::github::bridge::tick::{Ctx, run_once};
use crate::github::test_support::{MockResponse, MockServer, RecordedRequest};
use std::sync::{Arc, Mutex};

const TTL: i64 = 1800;
const NOW: i64 = 1_754_000_000;
const ISSUE: u64 = 7;

/// The shared issue, as GitHub would hold it: one comment list and one label
/// set that both instances read from and write to.
struct SharedIssue {
    comments: Vec<(u64, String)>,
    labels: Vec<String>,
    next_comment_id: u64,
    closed: bool,
}

impl SharedIssue {
    fn new() -> SharedIssue {
        SharedIssue {
            comments: Vec::new(),
            labels: vec!["agentflare".to_string()],
            next_comment_id: 1000,
            closed: false,
        }
    }

    fn issue_json(&self) -> String {
        let labels: Vec<String> = self
            .labels
            .iter()
            .map(|l| format!(r#"{{"name":{}}}"#, json_str(l)))
            .collect();
        format!(
            r#"{{"number":{ISSUE},"html_url":"u","state":"{}","title":"Do the thing","body":"","labels":[{}]}}"#,
            if self.closed { "closed" } else { "open" },
            labels.join(",")
        )
    }

    fn comments_json(&self) -> String {
        let items: Vec<String> = self
            .comments
            .iter()
            .map(|(id, body)| {
                format!(
                    r#"{{"id":{id},"user":{{"login":"u"}},"body":{}}}"#,
                    json_str(body)
                )
            })
            .collect();
        format!("[{}]", items.join(","))
    }

    fn markers_by(&self, owner: &str, action: &Action) -> usize {
        self.comments
            .iter()
            .filter_map(|(_, body)| Marker::parse(body))
            .filter(|m| m.owner == owner && m.action == *action)
            .count()
    }
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

/// A mock GitHub backed by one `SharedIssue`. Both instances' clients point
/// at it, so whatever one of them writes is what the other one reads.
fn shared_github(state: &Arc<Mutex<SharedIssue>>) -> MockServer {
    let state = Arc::clone(state);
    MockServer::start_with(move |req: &RecordedRequest| {
        let mut issue = state.lock().unwrap();
        let path = req.path.split('?').next().unwrap_or(&req.path).to_string();
        let body: serde_json::Value =
            serde_json::from_str(&req.body).unwrap_or(serde_json::Value::Null);

        match (req.method.as_str(), path.as_str()) {
            // The queue listing. A closed issue leaves the `state=open` queue.
            ("GET", "/repos/o/r/issues") => {
                let list = if issue.closed {
                    "[]".to_string()
                } else {
                    format!("[{}]", issue.issue_json())
                };
                MockResponse::json(200, &list)
            }
            ("GET", "/repos/o/r/issues/7/comments") => {
                MockResponse::json(200, &issue.comments_json())
            }
            ("POST", "/repos/o/r/issues/7/comments") => {
                let id = issue.next_comment_id;
                issue.next_comment_id += 1;
                let text = body["body"].as_str().unwrap_or_default().to_string();
                issue.comments.push((id, text));
                MockResponse::json(201, &format!(r#"{{"id":{id}}}"#))
            }
            ("POST", "/repos/o/r/issues/7/labels") => {
                for l in body["labels"].as_array().into_iter().flatten() {
                    if let Some(name) = l.as_str()
                        && !issue.labels.iter().any(|e| e == name)
                    {
                        issue.labels.push(name.to_string());
                    }
                }
                MockResponse::json(200, "[]")
            }
            ("DELETE", p) if p.starts_with("/repos/o/r/issues/7/labels/") => {
                let name = p.rsplit('/').next().unwrap_or_default().replace("%3A", ":");
                let had = issue.labels.contains(&name);
                issue.labels.retain(|l| *l != name);
                if had {
                    MockResponse::json(200, "[]")
                } else {
                    MockResponse::json(404, r#"{"message":"Label does not exist"}"#)
                }
            }
            ("PATCH", "/repos/o/r/issues/7") => {
                issue.closed = body["state"] == "closed";
                MockResponse::json(200, &issue.issue_json())
            }
            ("PATCH", p) if p.starts_with("/repos/o/r/issues/comments/") => {
                let id: u64 = p
                    .rsplit('/')
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let text = body["body"].as_str().unwrap_or_default().to_string();
                match issue.comments.iter_mut().find(|(cid, _)| *cid == id) {
                    Some(slot) => {
                        slot.1 = text;
                        MockResponse::json(200, &format!(r#"{{"id":{id}}}"#))
                    }
                    None => MockResponse::json(404, r#"{"message":"no such comment"}"#),
                }
            }
            _ => MockResponse::json(
                500,
                &format!(r#"{{"message":"unrouted {} {}"}}"#, req.method, req.path),
            ),
        }
    })
}

/// One bridge instance: its own item database, its own claim ledger, its own
/// owner id — sharing nothing but the GitHub server.
struct Instance {
    ctx: Ctx,
    conn: rusqlite::Connection,
    project_id: String,
}

impl Instance {
    fn new(server: &MockServer, owner: &str, max_claims: usize) -> Instance {
        let (conn, project_id) = crate::github::bridge::items::tests::tests_support_db();
        let ledger = rusqlite::Connection::open_in_memory().unwrap();
        crate::claims::migrate(&ledger).unwrap();
        Instance {
            ctx: Ctx {
                client: server.client(Some("tok")),
                repo: crate::github::RepoId {
                    owner: "o".into(),
                    repo: "r".into(),
                },
                config: BridgeConfig::from_values(
                    Some("1"),
                    None,
                    Some(&max_claims.to_string()),
                    None,
                    owner.to_string(),
                ),
                project_id: project_id.clone(),
                ledger,
            },
            conn,
            project_id,
        }
    }

    fn tick(&self, now: i64) {
        run_once(&self.ctx, &self.conn, now).expect("tick must not hard-error");
    }

    /// Whether this instance believes it is working the issue right now.
    fn holds(&self) -> bool {
        items::find_by_issue(&self.conn, &self.project_id, ISSUE)
            .is_some_and(|i| items::is_active(&self.conn, &i))
    }

    fn linked_items(&self) -> usize {
        agentflare_backend::item::list_by_project(&self.conn, &self.project_id)
            .unwrap()
            .into_iter()
            .filter(|i| i.external_id.as_deref() == Some("7"))
            .count()
    }
}

#[test]
fn two_instances_racing_for_one_issue_converge_on_a_single_holder() {
    let state = Arc::new(Mutex::new(SharedIssue::new()));
    let server = shared_github(&state);
    let a = Instance::new(&server, "a:1", 3);
    let b = Instance::new(&server, "b:2", 3);

    a.tick(NOW);
    b.tick(NOW);

    assert!(a.holds(), "the first claimant keeps the issue");
    assert!(
        !b.holds(),
        "the second instance must not also start work on it"
    );
    assert_eq!(b.linked_items(), 0, "a loser leaves no local trace at all");

    let issue = state.lock().unwrap();
    assert_eq!(
        issue.markers_by("b:2", &Action::Claim),
        0,
        "b saw a live claim before writing anything, so it posted nothing"
    );
    assert_eq!(issue.markers_by("a:1", &Action::Claim), 1);
}

#[test]
fn a_stalled_holder_loses_the_issue_after_the_ttl_and_cedes_on_its_next_tick() {
    let state = Arc::new(Mutex::new(SharedIssue::new()));
    let server = shared_github(&state);
    let a = Instance::new(&server, "a:1", 3);
    let b = Instance::new(&server, "b:2", 3);

    a.tick(NOW);
    assert!(a.holds());
    assert!(
        state
            .lock()
            .unwrap()
            .labels
            .contains(&"claimed:a:1".to_string()),
        "claiming labels the issue for a human scanning the list"
    );

    // `a` stops ticking entirely — a crashed or wedged instance. Once its
    // marker ages past the TTL the issue is free again.
    let after_ttl = NOW + TTL + 1;
    b.tick(after_ttl);
    assert!(
        b.holds(),
        "a stalled holder must not hold the queue forever"
    );

    // `a` comes back. It still thinks it holds the issue; GitHub says `b`
    // does, so it must give up rather than work in parallel.
    a.tick(after_ttl + 1);

    assert!(
        !a.holds(),
        "the stalled instance must drop the work it lost"
    );
    assert!(b.holds());
    let issue = state.lock().unwrap();
    assert!(
        !issue.labels.contains(&"claimed:a:1".to_string()),
        "and takes its `claimed:` label back off — otherwise every issue \
         accumulates one label per instance that ever touched it, and the \
         issue list shows work claimed by nobody"
    );
    assert!(issue.labels.contains(&"claimed:b:2".to_string()));
    assert_eq!(
        issue.markers_by("a:1", &Action::Cede),
        1,
        "and must say so on the issue, exactly once"
    );
}

#[test]
fn a_working_instance_keeps_its_claim_across_many_ttls_and_the_other_never_takes_it() {
    // THE regression test this file exists for. `a` claims the issue and
    // then just keeps ticking, without its content ever changing — an issue
    // being *worked* rather than *edited*, which is the normal case.
    //
    // Before the heartbeat existed nothing refreshed `a`'s marker, so at
    // t > TTL `a` would find itself not the holder, post "is ceding this —
    // another instance holds an earlier claim" (nobody did), cancel its own
    // item, and never reclaim it. No adversary and no second instance were
    // needed; `b` is here only to prove the issue is never actually loose.
    let state = Arc::new(Mutex::new(SharedIssue::new()));
    let server = shared_github(&state);
    let a = Instance::new(&server, "a:1", 3);
    let b = Instance::new(&server, "b:2", 3);

    a.tick(NOW);
    assert!(a.holds());

    let interval = 60;
    let span = TTL * 4;
    let mut t = NOW;
    while t <= NOW + span {
        t += interval;
        a.tick(t);
        b.tick(t);
        assert!(
            a.holds(),
            "a stopped holding its own live work at t = NOW + {}",
            t - NOW
        );
        assert!(
            !b.holds(),
            "the issue was never free at t = NOW + {}",
            t - NOW
        );
    }

    let issue = state.lock().unwrap();
    assert_eq!(
        issue.markers_by("a:1", &Action::Cede),
        0,
        "an instance must never cede work it is still doing"
    );
    assert_eq!(
        issue.markers_by("a:1", &Action::Claim),
        1,
        "the heartbeat must EDIT the one claim comment, not append a new \
         marker every half-TTL — over {} hours that would bury the issue",
        span / 3600
    );
    assert_eq!(
        issue.markers_by("b:2", &Action::Claim),
        0,
        "b must never have believed the issue was available"
    );
}

#[test]
fn a_finished_issue_is_closed_and_leaves_the_queue_for_both_instances() {
    let state = Arc::new(Mutex::new(SharedIssue::new()));
    let server = shared_github(&state);
    let a = Instance::new(&server, "a:1", 3);
    let b = Instance::new(&server, "b:2", 3);

    a.tick(NOW);
    let item = items::find_by_issue(&a.conn, &a.project_id, ISSUE).unwrap();
    let done = items::state_id_for_group(&a.conn, &a.project_id, "completed").unwrap();
    agentflare_backend::item::update_state(&a.conn, &item.id, &done).unwrap();

    a.tick(NOW + 60);

    {
        let issue = state.lock().unwrap();
        assert!(issue.closed, "completing the item must close the issue");
        assert_eq!(issue.markers_by("a:1", &Action::Done), 1);
    }

    // The issue is gone from the `state=open` queue, so `b` never sees it —
    // and a `done` marker means nobody may claim it even if they did.
    b.tick(NOW + 120);
    assert!(!b.holds());
    assert_eq!(b.linked_items(), 0);
}

#[test]
fn a_reclaim_after_the_previous_holder_ceded_reuses_the_ceding_instances_slot() {
    // Recovery in both directions: `a` loses the issue and cedes, then later
    // picks it up again rather than being locked out by the cancelled row
    // its own cede left behind.
    let state = Arc::new(Mutex::new(SharedIssue::new()));
    let server = shared_github(&state);
    let a = Instance::new(&server, "a:1", 3);
    let b = Instance::new(&server, "b:2", 3);

    a.tick(NOW);
    let after_ttl = NOW + TTL + 1;
    b.tick(after_ttl); // b takes over
    a.tick(after_ttl + 1); // a notices and cedes
    assert!(!a.holds());
    assert_eq!(a.linked_items(), 1, "the cede leaves the row behind");

    // `b` goes quiet and its own claim ages out.
    let much_later = after_ttl + TTL * 2;
    a.tick(much_later);

    assert!(
        a.holds(),
        "a cede must not lock this instance out of the issue permanently"
    );
    assert_eq!(
        a.linked_items(),
        1,
        "re-adopting must reuse the existing row — a second one linked to the \
         same issue would have the bridge writing one and reading the other"
    );
}
