use std::sync::Arc;

use crate::budget::{BudgetAllocation, error};
use crate::json::SendMessage;

/// Reserve at the send boundary, including multiple calls inside one logical step.
pub fn budgeted_sender(send: SendMessage) -> SendMessage {
    Arc::new(move |mut invocation| {
        let send = Arc::clone(&send);
        Box::pin(async move {
            let Some(budget) = invocation.budget.take() else {
                return send(invocation).await;
            };
            let reservation = budget
                .reserve(invocation.agent.clone())
                .await
                .map_err(|e| e.to_string())?;
            let id = reservation.id;
            let allocation = BudgetAllocation::new(reservation);
            invocation.allocation = Some(allocation.clone());
            // A dropped/timed-out future leaves its durable reservation charged.
            let result = match allocation.remaining_time().map_err(|e| e.to_string())? {
                Some(remaining) => tokio::time::timeout(remaining, send(invocation))
                    .await
                    .map_err(|_| error("elapsed allowance exhausted").to_string())?,
                None => send(invocation).await,
            };
            let update = budget
                .reconcile(id, allocation.cost())
                .await
                .map_err(|e| e.to_string())?;
            if update.state.overrun() {
                return Err(error("provider-reported cost exceeded its reservation").to_string());
            }
            result
        })
    })
}
