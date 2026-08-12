//! Host resource gate (item #435) — independent of, and alongside, the
//! per-agent cooldown check in `supervisor.rs`. `host_policy` is an injected
//! parameter rather than a live global read specifically so this test (and
//! every other test in the parent module) can't race a background sampler
//! thread or leak state across tests in the same binary.
//!
//! Split into its own file to keep `supervisor.rs` under the repo's LOC gate
//! (`scripts/loc-gate.sh`) — this module is otherwise part of `mod tests`,
//! and lives at the path Rust resolves `supervisor::tests::host_gate_tests`
//! to by default (no `#[path]` override, which would need a `..` traversal
//! through directories that don't exist and fails on Linux).

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

/// The review sweep's self-repair path has the same two independent gates
/// as the discovery tick. A host-pressure block there is retryable, so it
/// must report `Deferred` (counted as `waiting`) rather than `Skipped` —
/// otherwise the sweep log tells an operator the item was decided against
/// when in fact the next sweep will pick it up.
#[test]
fn self_repair_defers_rather_than_skips_when_the_host_gate_blocks() {
    let mcp = test_mcp();
    let queue = test_queue();
    let auth_conn = test_auth_conn();
    let item_id = seed_in_review_item(&mcp, Some("claude-code"));
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let label_id_by_name = seed_gate_label(&mcp);

    for policy in [
        agentflare_resource_gate::Policy::Throttled,
        agentflare_resource_gate::Policy::Paused {
            reason: agentflare_resource_gate::PauseReason::CpuPressure,
        },
    ] {
        let outcome = self_repair_or_gate(
            &mcp,
            &queue,
            &auth_conn,
            policy,
            &item,
            &["clippy".to_string()],
            &label_id_by_name,
        );

        assert!(
            matches!(outcome, SelfRepairOutcome::Deferred),
            "{} must defer, not skip — the pressure is expected to clear",
            policy.as_str()
        );
        assert!(
            queue.list(None).unwrap().is_empty(),
            "{} must not enqueue a repair job",
            policy.as_str()
        );
    }
}

/// The pre-existing per-agent cooldown is the same kind of retryable
/// deferral, and shares the new outcome.
#[test]
fn self_repair_defers_rather_than_skips_when_the_agent_is_cooling_down() {
    let mcp = test_mcp();
    let queue = test_queue();
    let auth_conn = test_auth_conn();
    let item_id = seed_in_review_item(&mcp, Some("claude-code"));
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let label_id_by_name = seed_gate_label(&mcp);
    crate::auth_db::set_cooldown(&auth_conn, "claude-code", "__default__", 30, "rate limit");

    let outcome = self_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        &["clippy".to_string()],
        &label_id_by_name,
    );

    assert!(matches!(outcome, SelfRepairOutcome::Deferred));
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
