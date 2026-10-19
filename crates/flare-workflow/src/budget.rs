//! Durable reservations for provider calls; missing usage never refunds money.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{StateStore, WorkflowData, WorkflowError, WorkflowResult, WorkflowRunId};

pub const ERROR_PREFIX: &str = "workflow budget: ";

pub fn error(message: impl Into<String>) -> WorkflowError {
    WorkflowError::Budget(message.into())
}

/// Dollar budgets require both a total and a reservation per invocation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BudgetLimits {
    pub max_cost_usd: Option<f64>,
    pub reserve_per_call_usd: Option<f64>,
    pub max_invocations: Option<u64>,
    pub max_elapsed_secs: Option<u64>,
}

fn micro_usd(value: f64) -> WorkflowResult<u64> {
    if !value.is_finite() || !(0.0..=1_000_000_000.0).contains(&value) {
        return Err(error(
            "cost must be finite, nonnegative and at most one billion USD",
        ));
    }
    Ok((value * 1_000_000.0).ceil() as u64)
}

impl BudgetLimits {
    pub fn validate(&self) -> WorkflowResult<()> {
        if self == &Self::default() {
            return Err(error("at least one limit is required"));
        }
        if self.max_cost_usd.is_some() != self.reserve_per_call_usd.is_some() {
            return Err(error(
                "max_cost_usd and reserve_per_call_usd must be specified together",
            ));
        }
        for value in [self.max_cost_usd, self.reserve_per_call_usd]
            .into_iter()
            .flatten()
        {
            micro_usd(value)?;
            if value < 0.000001 {
                return Err(error("dollar limits must be at least one microdollar"));
            }
        }
        if self.max_invocations == Some(0) || self.max_elapsed_secs == Some(0) {
            return Err(error("invocation and elapsed limits must be positive"));
        }
        if self
            .max_elapsed_secs
            .is_some_and(|v| v > i64::MAX as u64 / 1000)
        {
            return Err(error("elapsed limit is too large"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationCharge {
    pub id: u64,
    pub step_id: String,
    pub agent: String,
    pub logical_attempt: u32,
    pub reserved_micro_usd: Option<u64>,
    pub reported_micro_usd: Option<u64>,
    pub settled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetState {
    pub limits: BudgetLimits,
    pub started_at: DateTime<Utc>,
    pub invocations: Vec<InvocationCharge>,
}

#[derive(Debug, Clone)]
pub enum BudgetOperation {
    Snapshot,
    Reserve {
        step_id: String,
        agent: String,
        logical_attempt: u32,
    },
    Reconcile {
        id: u64,
        cost_usd: Option<f64>,
    },
}

#[derive(Debug, Clone)]
pub struct BudgetReservation {
    pub id: u64,
    pub reserved_micro_usd: Option<u64>,
    pub deadline: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct BudgetUpdate {
    pub state: BudgetState,
    pub reservation: Option<BudgetReservation>,
}

impl BudgetState {
    pub fn new(limits: BudgetLimits, started_at: DateTime<Utc>) -> WorkflowResult<Self> {
        limits.validate()?;
        let state = Self {
            limits,
            started_at,
            invocations: Vec::new(),
        };
        if state.limits.max_elapsed_secs.is_some() && state.deadline().is_none() {
            return Err(error("deadline exceeds supported timestamp range"));
        }
        Ok(state)
    }

    pub fn deadline(&self) -> Option<DateTime<Utc>> {
        self.started_at
            .checked_add_signed(chrono::Duration::try_seconds(
                i64::try_from(self.limits.max_elapsed_secs?).ok()?,
            )?)
    }

    fn committed(&self) -> u64 {
        self.invocations.iter().fold(0u64, |sum, call| {
            sum.saturating_add(
                call.reported_micro_usd
                    .or(call.reserved_micro_usd)
                    .unwrap_or(0),
            )
        })
    }

    pub fn cap_timeout(&self, timeout: Duration) -> WorkflowResult<Duration> {
        match self.deadline() {
            Some(deadline) => (deadline - Utc::now())
                .to_std()
                .ok()
                .filter(|remaining| !remaining.is_zero())
                .map(|remaining| timeout.min(remaining))
                .ok_or_else(|| error("elapsed allowance exhausted")),
            None => Ok(timeout),
        }
    }

    pub fn overrun(&self) -> bool {
        self.invocations.iter().any(|call| matches!((call.reserved_micro_usd, call.reported_micro_usd), (Some(reserved), Some(actual)) if actual > reserved))
    }

    pub fn apply(&mut self, operation: BudgetOperation) -> WorkflowResult<BudgetUpdate> {
        let mut reservation = None;
        match operation {
            BudgetOperation::Snapshot => {}
            BudgetOperation::Reserve {
                step_id,
                agent,
                logical_attempt,
            } => {
                self.limits.validate()?;
                if self
                    .deadline()
                    .is_some_and(|deadline| Utc::now() >= deadline)
                {
                    return Err(error("elapsed allowance exhausted"));
                }
                if self.overrun() {
                    return Err(error(
                        "a provider exceeded its reservation; further dispatch is blocked",
                    ));
                }
                if self
                    .limits
                    .max_invocations
                    .is_some_and(|max| self.invocations.len() as u64 >= max)
                {
                    return Err(error("invocation allowance exhausted"));
                }
                let reserved_micro_usd =
                    match (self.limits.max_cost_usd, self.limits.reserve_per_call_usd) {
                        (Some(max), Some(per_call)) => {
                            // Floor the ceiling and round charges up; fractional microdollars cannot add headroom.
                            let remaining = ((max * 1_000_000.0).floor() as u64)
                                .saturating_sub(self.committed());
                            if remaining == 0 {
                                return Err(error("dollar allowance exhausted"));
                            }
                            Some(micro_usd(per_call)?.min(remaining))
                        }
                        _ => None,
                    };
                let id = self.invocations.len() as u64 + 1;
                self.invocations.push(InvocationCharge {
                    id,
                    step_id,
                    agent,
                    logical_attempt,
                    reserved_micro_usd,
                    reported_micro_usd: None,
                    settled: false,
                });
                reservation = Some(BudgetReservation {
                    id,
                    reserved_micro_usd,
                    deadline: self.deadline(),
                });
            }
            BudgetOperation::Reconcile { id, cost_usd } => {
                let cost = cost_usd.map(micro_usd).transpose()?;
                let call = self
                    .invocations
                    .iter_mut()
                    .find(|call| call.id == id)
                    .ok_or_else(|| error("unknown reservation"))?;
                if call.settled && call.reported_micro_usd != cost {
                    return Err(error("reservation already reconciled with different usage"));
                }
                call.reported_micro_usd = cost;
                call.settled = true;
            }
        }
        Ok(BudgetUpdate {
            state: self.clone(),
            reservation,
        })
    }

    pub fn summary(&self) -> serde_json::Value {
        let reported = self.invocations.iter().fold(0u64, |sum, call| {
            sum.saturating_add(call.reported_micro_usd.unwrap_or(0))
        });
        let held = self
            .invocations
            .iter()
            .filter(|call| call.reported_micro_usd.is_none())
            .fold(0u64, |sum, call| {
                sum.saturating_add(call.reserved_micro_usd.unwrap_or(0))
            });
        serde_json::json!({
            "limits":self.limits,"started_at":self.started_at,"deadline":self.deadline(),
            "invocation_reservations":self.invocations.len(),"reported_cost_usd":reported as f64 / 1_000_000.0,
            "held_cost_usd":held as f64 / 1_000_000.0,
            "remaining_cost_usd":self.limits.max_cost_usd.map(|max| (max - self.committed() as f64 / 1_000_000.0).max(0.0)),
            "unknown_usage_count":self.invocations.iter().filter(|call|call.reported_micro_usd.is_none()).count(),
            "provider_overrun":self.overrun(),"invocations":self.invocations,
            "cost_scope":"Provider-reported usage; unknown or interrupted calls retain their reservations. Reservations count dispatch attempts, not confirmed provider starts."
        })
    }
}

#[async_trait]
trait BudgetAccess: Send + Sync {
    async fn apply(&self, operation: BudgetOperation) -> WorkflowResult<BudgetUpdate>;
}

struct StoredBudget<D: WorkflowData, S: StateStore<D>> {
    store: S,
    run_id: WorkflowRunId,
    _data: std::marker::PhantomData<D>,
}

#[async_trait]
impl<D: WorkflowData, S: StateStore<D>> BudgetAccess for StoredBudget<D, S> {
    async fn apply(&self, operation: BudgetOperation) -> WorkflowResult<BudgetUpdate> {
        self.store.budget_operation(self.run_id, operation).await
    }
}

/// Runtime access to a run's ledger; never serialized into step snapshots.
#[derive(Clone)]
pub struct BudgetHandle {
    access: Arc<dyn BudgetAccess>,
    step_id: String,
    logical_attempt: u32,
}

impl std::fmt::Debug for BudgetHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BudgetHandle")
    }
}

impl BudgetHandle {
    pub(crate) fn new<D: WorkflowData, S: StateStore<D> + 'static>(
        store: S,
        run_id: WorkflowRunId,
        step_id: String,
        logical_attempt: u32,
    ) -> Self {
        Self {
            access: Arc::new(StoredBudget {
                store,
                run_id,
                _data: std::marker::PhantomData,
            }),
            step_id,
            logical_attempt,
        }
    }

    pub(crate) async fn reserve(&self, agent: String) -> WorkflowResult<BudgetReservation> {
        self.access
            .apply(BudgetOperation::Reserve {
                step_id: self.step_id.clone(),
                agent,
                logical_attempt: self.logical_attempt,
            })
            .await?
            .reservation
            .ok_or_else(|| error("reservation missing"))
    }

    pub(crate) async fn reconcile(
        &self,
        id: u64,
        cost_usd: Option<f64>,
    ) -> WorkflowResult<BudgetUpdate> {
        self.access
            .apply(BudgetOperation::Reconcile { id, cost_usd })
            .await
    }
}

/// Per-call allowance and usage receipt shared with the existing send hook.
#[derive(Debug, Clone)]
pub struct BudgetAllocation {
    pub reservation: BudgetReservation,
    cost_usd: Arc<Mutex<Option<f64>>>,
}

impl BudgetAllocation {
    pub(crate) fn new(reservation: BudgetReservation) -> Self {
        Self {
            reservation,
            cost_usd: Arc::new(Mutex::new(None)),
        }
    }
    pub fn record_cost(&self, cost_usd: Option<f64>) {
        *self.cost_usd.lock().expect("budget usage receipt poisoned") = cost_usd;
    }
    pub(crate) fn cost(&self) -> Option<f64> {
        *self.cost_usd.lock().expect("budget usage receipt poisoned")
    }
    pub fn max_cost_usd(&self) -> Option<f64> {
        self.reservation
            .reserved_micro_usd
            .map(|micro| micro as f64 / 1_000_000.0)
    }
    pub fn remaining_time(&self) -> WorkflowResult<Option<Duration>> {
        self.reservation
            .deadline
            .map(|deadline| {
                (deadline - Utc::now())
                    .to_std()
                    .ok()
                    .filter(|remaining| !remaining.is_zero())
                    .ok_or_else(|| error("elapsed allowance exhausted"))
            })
            .transpose()
    }
}
