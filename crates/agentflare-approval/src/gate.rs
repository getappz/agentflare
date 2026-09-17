//! [`ApprovalGate`] — the async park/decide coordinator between a
//! classified command and a human decision.
//!
//! Flow: `intercept` classifies the command, consults the autonomy tier
//! ([`crate::policy::tier_action`]), then either allows, blocks, or parks a
//! `pending_approvals` row and waits (bounded by a TTL) for [`Self::decide`]
//! to be called. `decide` is race-safe: the channel surface and the
//! terminal surface both call it with the same `request_id`, and the store's
//! conditional `UPDATE ... WHERE decided_at IS NULL` makes the first caller
//! win (see `agentflare_store::approval::decide`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use agentflare_store::Store;
use parking_lot::{Mutex, RwLock};
use tokio::sync::oneshot;

use crate::classifier::{allow_key, classify_command};
use crate::policy::tier_action;
use crate::redact::redact_command;
use crate::types::{
    ApprovalDecision, AutonomyTier, CommandClass, DecidedVia, ExecutionOutcome, GateOutcome,
    Origin, PendingApproval, TierAction,
};

/// Default park window: 10 minutes, per the epic's fail-closed TTL.
pub const DEFAULT_TTL: Duration = Duration::from_secs(10 * 60);

pub struct ApprovalGate {
    store: Arc<Store>,
    tier: RwLock<AutonomyTier>,
    ttl: Duration,
    waiters: Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>,
}

impl ApprovalGate {
    pub fn new(store: Arc<Store>, tier: AutonomyTier) -> Self {
        Self::with_ttl(store, tier, DEFAULT_TTL)
    }

    pub fn with_ttl(store: Arc<Store>, tier: AutonomyTier, ttl: Duration) -> Self {
        Self {
            store,
            tier: RwLock::new(tier),
            ttl,
            waiters: Mutex::new(HashMap::new()),
        }
    }

    pub fn tier(&self) -> AutonomyTier {
        *self.tier.read()
    }

    pub fn set_tier(&self, tier: AutonomyTier) {
        *self.tier.write() = tier;
    }

    /// Intercept a command. Returns the request id when a `pending_approvals`
    /// row was persisted (so the caller can later call
    /// [`Self::record_execution`] once the command finishes) — `None` when no
    /// row exists (blocked, allowed outright, or the always-allow list hit).
    pub async fn intercept(&self, command: &str, origin: Origin) -> (GateOutcome, Option<String>) {
        let class = classify_command(command);
        match tier_action(self.tier(), class) {
            TierAction::Block => {
                return (
                    GateOutcome::Deny {
                        reason: format!(
                            "command classified as {} is blocked in {:?} tier",
                            class.as_str(),
                            self.tier()
                        ),
                    },
                    None,
                );
            }
            TierAction::Allow => return (GateOutcome::Allow, None),
            TierAction::Prompt => {}
        }

        // Interactive-only: background/cron turns are pre-authorized by
        // their own policy before this call is ever made — the gate never
        // silently prompts into the void, but it also never auto-approves
        // just because there's nobody to ask. A non-interactive origin
        // reaching `Prompt` here means the caller's own policy already
        // vetted it; the gate lets it through without persisting a row
        // (there is no human turn to audit against).
        if origin != Origin::Interactive {
            tracing::debug!(
                command_class = class.as_str(),
                origin = origin.as_str(),
                "[approval::gate] non-interactive origin pre-authorized, skipping prompt"
            );
            return (GateOutcome::Allow, None);
        }

        let key = allow_key(command);
        let allowlisted =
            agentflare_store::approval::allowlist_contains(&self.store, &key).unwrap_or(false);
        if allowlisted {
            return self.allow_via_allowlist(command, class, &key, origin).await;
        }

        self.park(command, class, &key, origin).await
    }

    async fn allow_via_allowlist(
        &self,
        command: &str,
        class: CommandClass,
        key: &str,
        origin: Origin,
    ) -> (GateOutcome, Option<String>) {
        let request_id = db_kit::ids::new_id();
        let now = db_kit::ids::now();
        let redacted = redact_command(command);
        let summary = summarize(command, class);
        if let Err(err) = agentflare_store::approval::insert_pending(
            &self.store,
            &request_id,
            class.as_str(),
            key,
            &summary,
            &redacted,
            origin.as_str(),
            now,
            None,
        ) {
            tracing::error!(error = %err, "[approval::gate] failed to persist allowlist audit row");
            return (GateOutcome::Allow, None);
        }
        let _ = agentflare_store::approval::decide(
            &self.store,
            &request_id,
            ApprovalDecision::ApproveOnce.as_str(),
            DecidedVia::Allowlist.as_str(),
            now,
        );
        (GateOutcome::Allow, Some(request_id))
    }

    async fn park(
        &self,
        command: &str,
        class: CommandClass,
        key: &str,
        origin: Origin,
    ) -> (GateOutcome, Option<String>) {
        let request_id = db_kit::ids::new_id();
        let now = db_kit::ids::now();
        let expires_at = Some(now + self.ttl.as_secs() as i64);
        let redacted = redact_command(command);
        let summary = summarize(command, class);

        // Register the waiter BEFORE persisting the row so a fast `decide`
        // call cannot resolve the request while no waiter exists.
        let (tx, rx) = oneshot::channel::<ApprovalDecision>();
        self.waiters.lock().insert(request_id.clone(), tx);

        if let Err(err) = agentflare_store::approval::insert_pending(
            &self.store,
            &request_id,
            class.as_str(),
            key,
            &summary,
            &redacted,
            origin.as_str(),
            now,
            expires_at,
        ) {
            self.waiters.lock().remove(&request_id);
            tracing::error!(error = %err, "[approval::gate] failed to persist pending row — failing closed");
            return (
                GateOutcome::Deny {
                    reason: format!(
                        "approval gate could not persist the request — denying for safety: {err}"
                    ),
                },
                None,
            );
        }

        tracing::info!(
            request_id = %request_id,
            command_class = class.as_str(),
            "[approval::gate] parked, waiting for a decision"
        );

        let outcome = match tokio::time::timeout(self.ttl, rx).await {
            Ok(Ok(decision)) => {
                if decision.is_approve() {
                    GateOutcome::Allow
                } else {
                    GateOutcome::Deny {
                        reason: "user denied the request".to_string(),
                    }
                }
            }
            Ok(Err(_canceled)) => {
                // Sender dropped without sending — treat as denial rather
                // than silently no-op.
                let now = db_kit::ids::now();
                let _ = agentflare_store::approval::decide(
                    &self.store,
                    &request_id,
                    ApprovalDecision::Deny.as_str(),
                    DecidedVia::Timeout.as_str(),
                    now,
                );
                GateOutcome::Deny {
                    reason: "approval channel closed before a decision was made".to_string(),
                }
            }
            Err(_elapsed) => {
                self.waiters.lock().remove(&request_id);
                let now = db_kit::ids::now();
                // Re-read-before-deny: a decision may have committed in the
                // store in the same instant the TTL elapsed. Try to deny;
                // `decide` no-ops (returns None) if the row is already
                // decided, in which case honor whatever was persisted
                // rather than overwrite it with a timeout deny.
                let denied = agentflare_store::approval::decide(
                    &self.store,
                    &request_id,
                    ApprovalDecision::Deny.as_str(),
                    DecidedVia::Timeout.as_str(),
                    now,
                );
                let persisted = match denied {
                    Ok(Some(_)) => Some(ApprovalDecision::Deny),
                    Ok(None) => agentflare_store::approval::get_decision(&self.store, &request_id)
                        .ok()
                        .flatten()
                        .and_then(|d| ApprovalDecision::from_str(&d)),
                    Err(_) => None,
                };
                if matches!(persisted, Some(d) if d.is_approve()) {
                    tracing::info!(
                        request_id = %request_id,
                        "[approval::gate] TTL race: persisted decision was approve, honoring it"
                    );
                    GateOutcome::Allow
                } else {
                    tracing::warn!(request_id = %request_id, "[approval::gate] approval timed out, denying");
                    GateOutcome::Deny {
                        reason: format!(
                            "approval for '{}' timed out after {}s",
                            summarize(command, class),
                            self.ttl.as_secs()
                        ),
                    }
                }
            }
        };

        (outcome, Some(request_id))
    }

    /// Apply a decision from a specific surface (channel card or terminal
    /// prompt). Returns `true` only if this call actually resolved the
    /// request — `false` means it lost the race (another surface already
    /// decided) or the request is unknown.
    pub fn decide(&self, request_id: &str, decision: ApprovalDecision, via: DecidedVia) -> bool {
        let now = db_kit::ids::now();
        let result = agentflare_store::approval::decide(
            &self.store,
            request_id,
            decision.as_str(),
            via.as_str(),
            now,
        );
        let Ok(Some(row)) = result else {
            return false;
        };
        if decision == ApprovalDecision::ApproveAlways {
            let added =
                agentflare_store::approval::allowlist_add(&self.store, &row.command_key, now);
            if let Err(err) = added {
                tracing::warn!(error = %err, key = %row.command_key, "[approval::gate] failed to persist always-allow grant");
            }
        }
        if let Some(tx) = self.waiters.lock().remove(request_id) {
            let _ = tx.send(decision);
        }
        true
    }

    /// Write-once terminal execution outcome for the audit trail. Best
    /// effort — logs but does not propagate a write failure, since the
    /// command has already run by the time this is called.
    pub fn record_execution(
        &self,
        request_id: &str,
        outcome: ExecutionOutcome,
        error: Option<&str>,
    ) {
        let now = db_kit::ids::now();
        match agentflare_store::approval::record_execution(
            &self.store,
            request_id,
            outcome.as_str(),
            error,
            now,
        ) {
            Ok(true) => {}
            Ok(false) => tracing::warn!(
                request_id,
                "[approval::gate] record_execution found no matching decided row"
            ),
            Err(err) => {
                tracing::error!(request_id, error = %err, "[approval::gate] record_execution write failed")
            }
        }
    }

    pub fn list_pending(&self) -> Result<Vec<PendingApproval>, agentflare_store::Error> {
        let now = db_kit::ids::now();
        let rows = agentflare_store::approval::list_pending(&self.store, now)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                Some(PendingApproval {
                    request_id: r.request_id,
                    command_class: CommandClass::from_str(&r.command_class)?,
                    action_summary: r.action_summary,
                    command_redacted: r.command_redacted,
                    origin: match r.origin.as_str() {
                        "interactive" => Origin::Interactive,
                        "background" => Origin::Background,
                        _ => Origin::Cron,
                    },
                    created_at: r.created_at,
                    expires_at: r.expires_at,
                })
            })
            .collect())
    }
}

fn summarize(command: &str, class: CommandClass) -> String {
    let redacted = redact_command(command);
    format!("[{}] {}", class.as_str(), redacted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;

    fn gate_with_ttl(tier: AutonomyTier, ttl: StdDuration) -> Arc<ApprovalGate> {
        let store = Arc::new(Store::open_memory().unwrap());
        Arc::new(ApprovalGate::with_ttl(store, tier, ttl))
    }

    #[tokio::test]
    async fn read_class_allows_without_parking() {
        let gate = gate_with_ttl(AutonomyTier::Supervised, StdDuration::from_secs(5));
        let (outcome, request_id) = gate.intercept("git status", Origin::Interactive).await;
        assert_eq!(outcome, GateOutcome::Allow);
        assert!(request_id.is_none());
    }

    #[tokio::test]
    async fn read_only_tier_blocks_write_outright() {
        let gate = gate_with_ttl(AutonomyTier::ReadOnly, StdDuration::from_secs(5));
        let (outcome, request_id) = gate.intercept("rm file.txt", Origin::Interactive).await;
        assert!(matches!(outcome, GateOutcome::Deny { .. }));
        assert!(request_id.is_none(), "a Block never persists a row");
    }

    #[tokio::test]
    async fn non_interactive_origin_is_preauthorized_without_parking() {
        let gate = gate_with_ttl(AutonomyTier::Supervised, StdDuration::from_secs(5));
        let (outcome, request_id) = gate.intercept("rm file.txt", Origin::Background).await;
        assert_eq!(outcome, GateOutcome::Allow);
        assert!(request_id.is_none());
    }

    #[tokio::test]
    async fn approve_once_resolves_the_parked_call() {
        let gate = gate_with_ttl(AutonomyTier::Supervised, StdDuration::from_secs(5));
        let g = gate.clone();
        let handle =
            tokio::spawn(async move { g.intercept("rm file.txt", Origin::Interactive).await });

        // Give the intercept task a moment to register the waiter + row.
        tokio::time::sleep(StdDuration::from_millis(20)).await;
        let request_id = loop {
            let pending = gate.list_pending().unwrap();
            if let Some(p) = pending.into_iter().next() {
                break p.request_id;
            }
            tokio::time::sleep(StdDuration::from_millis(5)).await;
        };
        assert!(gate.decide(
            &request_id,
            ApprovalDecision::ApproveOnce,
            DecidedVia::Terminal
        ));

        let (outcome, returned_id) = handle.await.unwrap();
        assert_eq!(outcome, GateOutcome::Allow);
        assert_eq!(returned_id, Some(request_id));
    }

    #[tokio::test]
    async fn deny_resolves_the_parked_call_as_deny() {
        let gate = gate_with_ttl(AutonomyTier::Supervised, StdDuration::from_secs(5));
        let g = gate.clone();
        let handle =
            tokio::spawn(async move { g.intercept("rm file.txt", Origin::Interactive).await });

        tokio::time::sleep(StdDuration::from_millis(20)).await;
        let request_id = gate.list_pending().unwrap().remove(0).request_id;
        assert!(gate.decide(&request_id, ApprovalDecision::Deny, DecidedVia::Channel));

        let (outcome, _) = handle.await.unwrap();
        assert!(matches!(outcome, GateOutcome::Deny { .. }));
    }

    #[tokio::test]
    async fn ttl_elapses_to_a_fail_closed_deny() {
        let gate = gate_with_ttl(AutonomyTier::Supervised, StdDuration::from_millis(50));
        let (outcome, request_id) = gate.intercept("rm file.txt", Origin::Interactive).await;
        assert!(matches!(outcome, GateOutcome::Deny { .. }));
        assert!(
            request_id.is_some(),
            "a timed-out row still exists for audit"
        );
    }

    #[tokio::test]
    async fn first_decision_wins_the_race_between_two_surfaces() {
        let gate = gate_with_ttl(AutonomyTier::Supervised, StdDuration::from_secs(5));
        let g = gate.clone();
        let handle =
            tokio::spawn(async move { g.intercept("rm file.txt", Origin::Interactive).await });

        tokio::time::sleep(StdDuration::from_millis(20)).await;
        let request_id = gate.list_pending().unwrap().remove(0).request_id;

        // Simulate the channel card and the terminal prompt racing to
        // answer the same request — only the first must win.
        let won_channel = gate.decide(
            &request_id,
            ApprovalDecision::ApproveOnce,
            DecidedVia::Channel,
        );
        let won_terminal = gate.decide(&request_id, ApprovalDecision::Deny, DecidedVia::Terminal);
        assert!(won_channel);
        assert!(
            !won_terminal,
            "the second surface to answer must lose the race"
        );

        let (outcome, _) = handle.await.unwrap();
        assert_eq!(
            outcome,
            GateOutcome::Allow,
            "the winning (approve) decision must be honored"
        );
    }

    #[tokio::test]
    async fn approve_always_persists_the_allowlist_and_skips_future_prompts() {
        let gate = gate_with_ttl(AutonomyTier::Supervised, StdDuration::from_secs(5));
        let g = gate.clone();
        let handle = tokio::spawn(async move {
            g.intercept("npm install left-pad", Origin::Interactive)
                .await
        });

        tokio::time::sleep(StdDuration::from_millis(20)).await;
        let request_id = gate.list_pending().unwrap().remove(0).request_id;
        assert!(gate.decide(
            &request_id,
            ApprovalDecision::ApproveAlways,
            DecidedVia::Terminal
        ));
        let (outcome, _) = handle.await.unwrap();
        assert_eq!(outcome, GateOutcome::Allow);

        // A subsequent call of the same command key must skip the prompt.
        let (outcome2, request_id2) = gate
            .intercept("npm install right-pad", Origin::Interactive)
            .await;
        assert_eq!(outcome2, GateOutcome::Allow);
        // Still recorded for audit, but pre-decided (no live park).
        assert!(request_id2.is_some());
        assert!(gate.list_pending().unwrap().is_empty());
    }

    #[tokio::test]
    async fn restart_keeps_pending_rows_visible() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        let request_id = {
            let store = Arc::new(Store::open_file(&path).unwrap());
            let gate = Arc::new(ApprovalGate::with_ttl(
                store,
                AutonomyTier::Supervised,
                StdDuration::from_secs(30),
            ));
            let g = gate.clone();
            let _handle =
                tokio::spawn(async move { g.intercept("rm file.txt", Origin::Interactive).await });
            tokio::time::sleep(StdDuration::from_millis(20)).await;
            gate.list_pending().unwrap().remove(0).request_id
            // gate (and its in-memory waiter) is dropped here, simulating a restart.
        };

        // A fresh process reopens the same store file.
        let store = Arc::new(Store::open_file(&path).unwrap());
        let gate =
            ApprovalGate::with_ttl(store, AutonomyTier::Supervised, StdDuration::from_secs(30));
        let pending = gate.list_pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].request_id, request_id);
    }

    #[tokio::test]
    async fn audit_row_is_written_after_execution() {
        let store = Arc::new(Store::open_memory().unwrap());
        let gate = Arc::new(ApprovalGate::with_ttl(
            store.clone(),
            AutonomyTier::Full,
            StdDuration::from_secs(5),
        ));
        // Full tier allows Write outright, but still persists an audit row
        // for Network/Install/Destructive via the parked path — use a
        // Network command here to exercise record_execution end-to-end.
        let g = gate.clone();
        let handle = tokio::spawn(async move {
            g.intercept("curl http://example.com", Origin::Interactive)
                .await
        });
        tokio::time::sleep(StdDuration::from_millis(20)).await;
        let request_id = gate.list_pending().unwrap().remove(0).request_id;
        gate.decide(
            &request_id,
            ApprovalDecision::ApproveOnce,
            DecidedVia::Terminal,
        );
        let (outcome, _) = handle.await.unwrap();
        assert_eq!(outcome, GateOutcome::Allow);

        gate.record_execution(&request_id, ExecutionOutcome::Success, None);
        let audits = agentflare_store::approval::list_recent_decisions(&store, 10).unwrap();
        let row = audits.iter().find(|r| r.request_id == request_id).unwrap();
        assert_eq!(row.execution_outcome.as_deref(), Some("success"));
    }
}
