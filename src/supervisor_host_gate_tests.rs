//! Host resource gate (item #435) — independent of, and alongside, the
//! per-agent cooldown check in `supervisor.rs`. `host_policy` is an injected
//! parameter rather than a live global read specifically so this test (and
//! every other test in the parent module) can't race a background sampler
//! thread or leak state across tests in the same binary.
//!
//! Split into its own file to keep `supervisor.rs` under the repo's LOC gate
//! (`scripts/loc-gate.sh`) — this module is otherwise part of `mod tests`.

use super::*;

#[test]
fn host_gate_throttled_is_skipped_not_dispatched() {
    let mcp = test_mcp();
    let queue = test_queue();
    let auth_conn = test_auth_conn();
    let item_id = seed_ready_item(&mcp, Some("claude-code"));

    let result = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Throttled,
    );

    assert_eq!(result.dispatched, 0);
    assert_eq!(result.skipped, 0);
    assert_eq!(
        result.waiting, 1,
        "a throttled host must count the item as waiting, not vanish it"
    );
    assert!(
        queue.list(None).unwrap().is_empty(),
        "a throttled host must not dispatch"
    );

    let labels = mcp
        .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
        .unwrap();
    assert!(
        labels_contain_name(&mcp, &labels, "ready-for-work"),
        "the item must stay ready-for-work so a later tick can pick it up once the host recovers"
    );
}

#[test]
fn host_gate_paused_is_skipped_not_dispatched() {
    let mcp = test_mcp();
    let queue = test_queue();
    let auth_conn = test_auth_conn();
    seed_ready_item(&mcp, Some("claude-code"));

    let result = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Paused {
            reason: agentflare_resource_gate::PauseReason::CpuPressure,
        },
    );

    assert_eq!(result.dispatched, 0);
    assert_eq!(result.waiting, 1);
    assert!(queue.list(None).unwrap().is_empty());
}

#[test]
fn host_gate_aggressive_does_not_block_an_otherwise_eligible_item() {
    let mcp = test_mcp();
    let queue = test_queue();
    let auth_conn = test_auth_conn();
    seed_ready_item(&mcp, Some("claude-code"));

    let result = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Aggressive,
    );

    assert_eq!(
        result.dispatched, 1,
        "Aggressive must dispatch exactly like Normal — it's a bypass, not a boost"
    );
}
