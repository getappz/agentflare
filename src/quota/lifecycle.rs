//! A goal's lifecycle: active work, paused, blocked on a human ("gated"),
//! or done. Persisted as the `lifecycle` field of `metadata.goal` on the
//! goal item; transitions are applied here and written back by the caller.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalLifecycle {
    Active,
    Paused,
    Gated,
    Done,
}

// Only `Gate` has a caller today (decide_for_supervisor's Ask branch). The
// rest complete the state machine for the quota MCP tool this item's spec
// calls for, which isn't wired up yet.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleEvent {
    Pause,
    Resume,
    Gate,
    Clear,
    Complete,
}

impl GoalLifecycle {
    /// Applies one event, returning the new state or an error naming the
    /// illegal transition. Never mutates in place — the caller decides
    /// whether/how to persist the result.
    pub fn apply(self, event: LifecycleEvent) -> Result<GoalLifecycle, String> {
        use GoalLifecycle::*;
        use LifecycleEvent::*;
        match (self, event) {
            (Active, Pause) => Ok(Paused),
            (Active, Gate) => Ok(Gated),
            (Active, Complete) => Ok(Done),
            (Paused, Resume) => Ok(Active),
            (Paused, Complete) => Ok(Done),
            (Gated, Clear) => Ok(Active),
            (state, event) => Err(format!(
                "illegal lifecycle transition: {event:?} from {state:?}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_pauses_gates_and_completes() {
        assert_eq!(
            GoalLifecycle::Active.apply(LifecycleEvent::Pause),
            Ok(GoalLifecycle::Paused)
        );
        assert_eq!(
            GoalLifecycle::Active.apply(LifecycleEvent::Gate),
            Ok(GoalLifecycle::Gated)
        );
        assert_eq!(
            GoalLifecycle::Active.apply(LifecycleEvent::Complete),
            Ok(GoalLifecycle::Done)
        );
    }

    #[test]
    fn paused_resumes_or_completes() {
        assert_eq!(
            GoalLifecycle::Paused.apply(LifecycleEvent::Resume),
            Ok(GoalLifecycle::Active)
        );
        assert_eq!(
            GoalLifecycle::Paused.apply(LifecycleEvent::Complete),
            Ok(GoalLifecycle::Done)
        );
    }

    #[test]
    fn gated_only_clears_to_active() {
        assert_eq!(
            GoalLifecycle::Gated.apply(LifecycleEvent::Clear),
            Ok(GoalLifecycle::Active)
        );
        assert!(GoalLifecycle::Gated
            .apply(LifecycleEvent::Complete)
            .is_err());
    }

    #[test]
    fn done_is_terminal() {
        for event in [
            LifecycleEvent::Pause,
            LifecycleEvent::Resume,
            LifecycleEvent::Gate,
            LifecycleEvent::Clear,
            LifecycleEvent::Complete,
        ] {
            assert!(
                GoalLifecycle::Done.apply(event).is_err(),
                "{event:?} must not leave Done"
            );
        }
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        assert!(GoalLifecycle::Active.apply(LifecycleEvent::Resume).is_err());
        assert!(GoalLifecycle::Active.apply(LifecycleEvent::Clear).is_err());
        assert!(GoalLifecycle::Paused.apply(LifecycleEvent::Gate).is_err());
    }

    #[test]
    fn lifecycle_round_trips_through_json_as_snake_case() {
        let json = serde_json::to_string(&GoalLifecycle::Gated).unwrap();
        assert_eq!(json, "\"gated\"");
        assert_eq!(
            serde_json::from_str::<GoalLifecycle>(&json).unwrap(),
            GoalLifecycle::Gated
        );
    }
}
