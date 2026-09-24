//! Process-wide "has this job been cancelled?" registry, keyed by job id.
//! `WorkerPool` registers a check for an in-process job's lifetime; code deep
//! inside that job -- notably the agent-CLI wait loop, which runs on a
//! different (`spawn_blocking`) thread than the executor and only receives
//! the job's claim-owner string (`<agent>:<job-id>`) -- looks it up by id via
//! `job_cancelled`, so no queue handle or thread-local has to survive the
//! thread hops in between.
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

/// Failure text for a run stopped because its job was cancelled -- the item
/// was reassigned, or an operator cancelled or paused it. Callers match on
/// it (`contains`) to treat the stop as deliberate rather than a failure.
pub const CANCELLED_MESSAGE: &str =
    "cancelled: job stopped on request (item reassigned, cancelled or paused)";

type Check = Arc<dyn Fn() -> bool + Send + Sync>;

static CHECKS: LazyLock<Mutex<HashMap<String, (u64, Check)>>> = LazyLock::new(Default::default);
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(0);

/// Unregisters the job's check on drop.
#[must_use = "the check is unregistered as soon as the guard is dropped"]
pub struct Registration {
    id: String,
    token: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        // Only the registration this guard made: an abandoned (timed-out)
        // executor thread can outlive its attempt, and the retry of the same
        // job id registers a fresh check that this stale guard must not drop.
        let mut checks = CHECKS.lock();
        if checks
            .get(&self.id)
            .is_some_and(|(token, _)| *token == self.token)
        {
            checks.remove(&self.id);
        }
    }
}

/// Registers `check` as job `id`'s cancellation check until the returned
/// guard drops.
pub fn register(id: &str, check: impl Fn() -> bool + Send + Sync + 'static) -> Registration {
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    CHECKS
        .lock()
        .insert(id.to_string(), (token, Arc::new(check)));
    Registration {
        id: id.to_string(),
        token,
    }
}

/// Whether job `id` is executing in this process right now (its check is
/// registered for exactly the lifetime of its executor). Lets a sweep that
/// judges claim owners from the outside never mistake this process's own
/// live job for a dead one.
pub fn is_registered(id: &str) -> bool {
    CHECKS.lock().contains_key(id)
}

/// True when job `id` is registered and reports cancelled; false for an
/// unknown id (a check failure must never kill live work).
pub fn job_cancelled(id: &str) -> bool {
    // Clone the Arc out so the (DB-hitting) check runs without the map lock.
    let check = CHECKS.lock().get(id).map(|(_, check)| check.clone());
    check.is_some_and(|check| check())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_cancelled_only_while_registered() {
        assert!(!job_cancelled("cancel-test-job"));
        let guard = register("cancel-test-job", || true);
        assert!(job_cancelled("cancel-test-job"));
        assert!(!job_cancelled("some-other-job"));
        drop(guard);
        assert!(!job_cancelled("cancel-test-job"), "unregistered on drop");
    }

    #[test]
    fn a_stale_guard_does_not_unregister_a_newer_registration_of_the_same_job() {
        let stale = register("cancel-retry-job", || false);
        let _retry = register("cancel-retry-job", || true);
        drop(stale);
        assert!(
            job_cancelled("cancel-retry-job"),
            "the retry's check survives"
        );
    }
}
