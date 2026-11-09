use chrono::{DateTime, Utc};
use flare_insights::forensics::{compute, normalise_user_text, ForensicsOptions};
use flare_insights::*;
use serde_json::Value;

fn now() -> DateTime<Utc> {
    "2026-01-08T00:00:00Z".parse().unwrap()
}

fn load(only: &dyn Fn(&str) -> bool) -> InsightsStore {
    let raw = include_str!("fixtures/forensics/input.json");
    let v: Value = serde_json::from_str(raw).unwrap();
    let store = InsightsStore::open_in_memory().unwrap();
    for s in v["sessions"].as_array().unwrap() {
        let source: SessionSource = serde_json::from_value(s["source"].clone()).unwrap();
        let id = s["id"].as_str().unwrap();
        if !only(id) {
            continue;
        }
        store
            .upsert_session(&Session {
                id: id.into(),
                source,
                project: "p".into(),
                project_path: None,
                title: None,
                model: None,
                status: SessionStatus::Completed,
                awaiting_reason: None,
                started_at: None,
                updated_at: s["updated_at"].as_str().unwrap().parse().unwrap(),
                ended_at: None,
                duration_secs: None,
                tokens: TokenUsage {
                    input: s["input"].as_u64().unwrap(),
                    output: 0,
                    cache_read: s["read"].as_u64().unwrap(),
                    cache_write: s["write"].as_u64().unwrap(),
                    reasoning: 0,
                },
                cost: s["cost"].as_f64().map(|total_usd| Cost {
                    total_usd,
                    input_usd: 0.0,
                    output_usd: 0.0,
                    cache_read_usd: 0.0,
                    cache_write_usd: 0.0,
                }),
                cost_basis: serde_json::from_value(s["basis"].clone()).unwrap(),
                pricing_key: None,
                turn_count: 0,
                tool_call_count: 0,
                subagent_count: 0,
                tags: vec![],
                starred: false,
                pid: None,
                cwd: s["cwd"].as_str().map(String::from),
            })
            .unwrap();
    }
    for t in v["turns"].as_array().unwrap() {
        let sid = t["session"].as_str().unwrap();
        if !only(sid) {
            continue;
        }
        let seq = t["seq"].as_u64().unwrap() as u32;
        store
            .upsert_turn(&Turn {
                id: format!("{sid}-{seq}"),
                session_id: sid.into(),
                seq,
                user_text: t["user_text"].as_str().map(String::from),
                assistant_text: None,
                started_at: None,
                ended_at: None,
                tokens: None,
                cost_usd: None,
            })
            .unwrap();
    }
    for t in v["tools"].as_array().unwrap() {
        let sid = t["session"].as_str().unwrap();
        if !only(sid) {
            continue;
        }
        store
            .upsert_tool_call(&ToolCall {
                id: t["id"].as_str().unwrap().into(),
                session_id: sid.into(),
                turn_seq: t["turn_seq"].as_u64().unwrap() as u32,
                name: t["name"].as_str().unwrap().into(),
                input: Value::Null,
                output: t["output_len"].as_u64().map(|n| "x".repeat(n as usize)),
                status: t["status"].as_str().unwrap().into(),
                duration_ms: None,
                created_at: None,
            })
            .unwrap();
    }
    store
}

fn finding<'a>(r: &'a Value, id: &str) -> &'a Value {
    r["body"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == id)
        .unwrap()
}

#[test]
fn forensics_golden_matches() {
    let store = load(&|_| true);
    let report =
        serde_json::to_value(compute(&store, &Default::default(), now()).unwrap()).unwrap();
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/forensics/expected.json"
    );
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(path, serde_json::to_string_pretty(&report).unwrap() + "\n").unwrap();
    }
    let expected: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(report, expected);
}

#[test]
fn unpriced_session_is_counted_not_shared() {
    let store = load(&|id| matches!(id, "c1" | "c2" | "c3"));
    let r = serde_json::to_value(compute(&store, &Default::default(), now()).unwrap()).unwrap();
    assert_eq!(r["coverage"]["unpriced_sessions"], 1);
    let v = &finding(&r, "cost_concentration")["value"];
    assert_eq!(v["priced_sessions"], 2);
    assert_eq!(v["total_usd"], 12.0);
    assert_eq!(v["top5_share"], 1.0);
}

#[test]
fn codex_streaks_unavailable() {
    let store = load(&|id| id == "x1");
    let r = serde_json::to_value(compute(&store, &Default::default(), now()).unwrap()).unwrap();
    let f = finding(&r, "failed_tool_streaks");
    assert!(f["value"].is_null());
    assert_eq!(f["unavailable_reason"], "tool_failure_status_not_recorded");
}

#[test]
fn normalisation_skips_short_text() {
    assert_eq!(normalise_user_text("ok thanks"), None);
    assert_eq!(
        normalise_user_text("<system-reminder>long long long long long</system-reminder> hi"),
        None
    );
    assert_eq!(
        normalise_user_text("  Please   RUN the tests\n now ").as_deref(),
        Some("please run the tests now")
    );
}

fn run(store: &InsightsStore, opts: ForensicsOptions) -> Value {
    serde_json::to_value(compute(store, &opts, now()).unwrap()).unwrap()
}

#[test]
fn streaks_require_failed_first_and_same_name() {
    // c3: Bash ok,Bash failed (first not failed) and Write failed,Edit ok (name changes).
    let store = load(&|id| id == "c3");
    let r = run(&store, Default::default());
    assert_eq!(
        finding(&r, "failed_tool_streaks")["value"],
        serde_json::json!([])
    );
    // c1: Bash failed,Bash ok is a streak of length 2.
    let store = load(&|id| id == "c1");
    let r = run(&store, Default::default());
    assert_eq!(finding(&r, "failed_tool_streaks")["value"][0]["length"], 2);
}

#[test]
fn cache_not_recorded_for_cursor_and_mix_in_coverage() {
    let store = load(&|_| true);
    let r = run(&store, Default::default());
    let f = finding(&r, "cache_reuse.cursor");
    assert!(f["value"].is_null());
    assert_eq!(f["unavailable_reason"], "cache_not_recorded");
    assert_eq!(r["coverage"]["cost_basis_mix"]["estimated"], 3);
}

#[test]
fn project_source_and_session_filters_apply() {
    let store = load(&|_| true);
    let r = run(
        &store,
        ForensicsOptions {
            project_prefix: Some("/w/p".into()),
            ..Default::default()
        },
    );
    assert_eq!(r["coverage"]["sessions_in_window"], 4);
    assert!(r["body"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| f["id"] != "cache_reuse.cursor"));
    let r = run(
        &store,
        ForensicsOptions {
            source: Some("codex".into()),
            ..Default::default()
        },
    );
    assert_eq!(r["coverage"]["sessions_in_window"], 1);
    let r = run(
        &store,
        ForensicsOptions {
            session_id: Some("c2".into()),
            ..Default::default()
        },
    );
    assert_eq!(r["coverage"]["sessions_in_window"], 1);
    assert_eq!(r["coverage"]["eligible_turns"], 2);
}

#[test]
fn repeated_text_evidence_lists_every_session() {
    let store = load(&|_| true);
    let r = run(&store, Default::default());
    let ev = finding(&r, "repeated_user_text.cross_session")["evidence"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(ev, 2);
}

#[test]
fn streak_can_start_after_a_success_in_the_same_tool_run() {
    // c3 already has Bash ok, Bash failed; a following Bash ok makes failed, ok a streak.
    let store = load(&|id| id == "c3");
    store
        .upsert_tool_call(&ToolCall {
            id: "t12".into(),
            session_id: "c3".into(),
            turn_seq: 1,
            name: "Bash".into(),
            input: Value::Null,
            output: None,
            status: "ok".into(),
            duration_ms: None,
            created_at: None,
        })
        .unwrap();
    let r = run(&store, Default::default());
    let value = &finding(&r, "failed_tool_streaks")["value"][0];
    assert_eq!(value["length"], 2);
    assert_eq!(value["tool"], "Bash");
}

#[test]
fn huge_days_does_not_panic() {
    let store = load(&|_| true);
    let opts = ForensicsOptions {
        days: u32::MAX,
        ..Default::default()
    };
    assert_eq!(run(&store, opts)["coverage"]["sessions_in_window"], 6);
}

#[test]
fn within_session_repeat_counts_only_sessions_that_repeated() {
    // c1 says it twice, c2 once: within-session is c1 alone, cross-session is both.
    let store = load(&|_| true);
    let r = run(&store, Default::default());
    let within = finding(&r, "repeated_user_text.within_session");
    assert_eq!(within["value"][0]["count"], 2);
    assert_eq!(within["value"][0]["sessions"], 1);
    let cross = finding(&r, "repeated_user_text.cross_session");
    assert_eq!(cross["value"][0]["count"], 3);
}
