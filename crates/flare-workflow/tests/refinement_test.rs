use flare_workflow::refinement::*;

fn request() -> RefinementRequest {
    serde_json::from_value(serde_json::json!({
        "task_id":"t", "domain":"rust", "prompt":"fix it", "repository":"repo",
        "solver":{"agent":"codex","model":"test"},
        "improver":{"agent":"codex","model":"test"},
        "mode":"parallel", "k":1, "checks":[["cargo","test"]]
    }))
    .unwrap()
}

fn tree() -> HarnessTree {
    HarnessTree::from([("AGENTS.md".into(), "Work carefully.".into())])
}

#[test]
fn request_rejects_invalid_limits_and_models() {
    let mut r = request();
    r.validate().unwrap();
    r.k = 0;
    assert!(r.validate().is_err());
    r.k = 17;
    assert!(r.validate().is_err());
    r.k = 1;
    r.solver.model.clear();
    assert!(r.validate().is_err());
    r.solver.model = "test".into();
    r.limits.max_calls = 0;
    assert!(r.validate().is_err());
    r.limits = Limits::default();
    r.limits.max_cost_usd = Some(f64::NAN);
    assert!(r.validate().is_err());
}

#[test]
fn request_requires_nonempty_acceptance_argv() {
    let mut r = request();
    r.checks.clear();
    assert!(r.validate().is_err());
    r.checks = vec![vec![String::new()]];
    assert!(r.validate().is_err());
}

#[test]
fn tree_rejects_portable_path_escapes() {
    for path in [
        "C:/x",
        "C:x",
        "/x",
        "../x",
        "a/../x",
        "\\\\host\\x",
        "NUL",
        "file:stream",
        ".agents/skills/con/SKILL.md",
        "AGENTS.md/",
        ".agents//skills/x/SKILL.md",
    ] {
        let mut t = tree();
        t.insert(path.into(), "bad".into());
        assert!(validate_tree(&t).is_err(), "accepted {path}");
    }
}

#[test]
fn tree_enforces_size_and_skill_contract() {
    let mut t = tree();
    validate_tree(&t).unwrap();
    t.insert("AGENTS.md".into(), "x".repeat(8001));
    assert!(validate_tree(&t).is_err());
    t.insert("AGENTS.md".into(), String::new());
    assert!(validate_tree(&t).is_err());
    t = tree();
    t.insert(
        ".agents/skills/review/SKILL.md".into(),
        "---\ndescription: Review code\n---\nCheck correctness".into(),
    );
    assert!(validate_tree(&t).is_err());
    t.insert(
        "AGENTS.md".into(),
        "Read .agents/skills/review/SKILL.md when reviewing.".into(),
    );
    validate_tree(&t).unwrap();
}

#[test]
fn tree_rejects_unclosed_frontmatter() {
    let mut t = tree();
    t.insert(
        "AGENTS.md".into(),
        "Read .agents/skills/review/SKILL.md when reviewing.".into(),
    );
    t.insert(
        ".agents/skills/review/SKILL.md".into(),
        "---\ndescription: review\nNo closing frontmatter".into(),
    );
    assert!(validate_tree(&t).is_err());
}

use flare_workflow::{SqliteStore, WorkflowEngine, WorkflowRunId};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
struct Scripted {
    seen: Mutex<Vec<Invocation>>,
    saved: Mutex<Vec<Experience>>,
    bad_proposal: bool,
    all_fail: bool,
    finals_fail: bool,
    bad_selection: bool,
    choose_final: bool,
    bad_reflection: bool,
    transport_once: std::sync::atomic::AtomicBool,
    cancelled: std::sync::atomic::AtomicBool,
}
#[async_trait::async_trait]
impl RefinementBackend for Scripted {
    async fn execute(&self, inv: Invocation) -> Result<Receipt, RefinementError> {
        assert!(!inv.prompt.contains("PRIVATE_CHECK_SENTINEL"));
        let mut seen = self.seen.lock().unwrap();
        if seen.len() >= inv.limits.max_calls as usize {
            return Err(RefinementError::Exhausted("calls".into()));
        }
        seen.push(inv.clone());
        let mut r = Receipt {
            id: inv.id.clone(),
            cost_usd: Some(0.01),
            ..Receipt::default()
        };
        if inv.role == Role::General
            && self
                .transport_once
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            r.error = Some("transient transport failure".into());
            return Ok(r);
        }
        match inv.role {
            Role::Probe => {
                r.reply = format!("Answer from {}", inv.id);
                r.evidence = format!("artifact from {}", inv.id);
                r.accepted = Some(!self.all_fail && !(self.finals_fail && inv.id.starts_with("compose")));
                r.artifact_identity = Some("artifact".into());
                r.private_checks = "PRIVATE_CHECK_SENTINEL".into();
            }
            Role::Select => r.reply = if self.bad_selection { "not json".into() } else if self.choose_final {
                let prompt: serde_json::Value = serde_json::from_str(&inv.prompt).unwrap();
                let chosen = prompt["candidates"].as_array().unwrap().iter().find(|c|c["answer"].as_str().unwrap().contains("compose")).unwrap();
                serde_json::json!({"choice":chosen["id"],"reason":"demonstrated fixture improvement"}).to_string()
            } else { r#"{"choice":"abstain","reason":"No demonstrated gain"}"#.into() },
            Role::Reflect => r.reply = serde_json::json!({"cue":"similar task", "lesson":"keep evidence", "evidence":"observed a real probe", "trial_ids":[if self.bad_reflection {"invented"} else {"base1"}], "confidence":"observed"}).to_string(),
            _ => {
                let t = if self.bad_proposal { HarnessTree::from([("../escape".into(), "bad".into())]) } else {
                    HarnessTree::from([("AGENTS.md".into(), format!("Work carefully. improved by {}", inv.id))])
                };
                r.reply = serde_json::json!({"tree":t,"notes":"observed change"}).to_string();
            }
        }
        Ok(r)
    }
    async fn load_experiences(
        &self,
        project: &str,
        domain: &str,
    ) -> Result<Vec<Experience>, RefinementError> {
        Ok(self
            .saved
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.project == project && e.domain == domain)
            .cloned()
            .collect())
    }
    async fn save_experience(
        &self,
        _: WorkflowRunId,
        e: &Experience,
    ) -> Result<(), RefinementError> {
        self.saved.lock().unwrap().push(e.clone());
        Ok(())
    }
    async fn check_active(&self, _: WorkflowRunId) -> Result<(), RefinementError> {
        if self.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            Err(RefinementError::Cancelled)
        } else {
            Ok(())
        }
    }
}

async fn run(r: RefinementRequest, backend: Arc<Scripted>) -> RefinementData {
    let wf = build_refinement_workflow(&r, backend).unwrap();
    let id = wf.id.clone();
    let engine = WorkflowEngine::with_store(SqliteStore::open_memory().unwrap());
    engine.register_workflow(wf).unwrap();
    let run = engine
        .start_workflow(
            id,
            RefinementData::new(r, "frozen".into(), tree()).unwrap(),
            String::new(),
        )
        .await
        .unwrap();
    engine
        .wait_for_completion(run, "refinement", Duration::from_secs(15))
        .await
        .unwrap();
    engine.get_status(run).await.unwrap().context.data
}

#[tokio::test]
async fn parallel_runs_4k_trials_and_3k_stages() {
    for (k, trials, stages) in [(1, 4, 3), (2, 8, 6)] {
        let mut r = request();
        r.k = k;
        let backend = Arc::new(Scripted::default());
        let data = run(r, backend.clone()).await;
        assert_eq!((data.trials.len(), data.stages.len()), (trials, stages));
        assert!(data.adoption.unwrap().is_baseline);
        assert_eq!(backend.saved.lock().unwrap().len(), 1);
        assert!(
            backend
                .seen
                .lock()
                .unwrap()
                .iter()
                .filter(|i| i.role == Role::Compose)
                .all(|i| i.prompt.contains("artifact from"))
        );
    }
}

#[tokio::test]
async fn full_loop_runs_with_the_in_memory_store() {
    let r = request();
    let wf = build_refinement_workflow(&r, Arc::new(Scripted::default())).unwrap();
    let id = wf.id.clone();
    let engine = WorkflowEngine::new();
    engine.register_workflow(wf).unwrap();
    let run = engine
        .start_workflow(
            id,
            RefinementData::new(r, "frozen".into(), tree()).unwrap(),
            String::new(),
        )
        .await
        .unwrap();
    engine
        .wait_for_completion(run, "refinement", Duration::from_secs(15))
        .await
        .unwrap();
    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.context.data.trials.len(), 4);
    assert!(state.context.data.adoption.unwrap().is_baseline);
}

#[tokio::test]
async fn failed_finals_do_not_displace_successful_baseline() {
    for mode in [RefinementMode::Parallel, RefinementMode::Sequential] {
        let mut r = request();
        r.mode = mode;
        let data = run(
            r,
            Arc::new(Scripted {
                finals_fail: true,
                ..Default::default()
            }),
        )
        .await;
        assert!(data.adoption.unwrap().is_baseline);
        assert!(
            data.trials
                .iter()
                .filter(|t| t.label.starts_with("compose"))
                .all(|t| !t.receipt.successful())
        );
    }
}

#[tokio::test]
async fn valid_anonymous_selection_adopts_the_exact_successful_revision() {
    let data = run(
        request(),
        Arc::new(Scripted {
            choose_final: true,
            ..Default::default()
        }),
    )
    .await;
    let adoption = data.adoption.as_ref().unwrap();
    assert!(!adoption.is_baseline);
    let trial = data
        .trials
        .iter()
        .find(|t| t.label == adoption.trial_id)
        .unwrap();
    assert!(trial.receipt.successful());
    assert_eq!(data.current, trial.tree);
    assert_ne!(data.current, data.base);
    assert_eq!(adoption.harness_identity, harness_identity(&data.current));
}

#[tokio::test]
async fn sequential_carries_composition_and_history() {
    let mut r = request();
    r.mode = RefinementMode::Sequential;
    r.k = 2;
    let backend = Arc::new(Scripted::default());
    let data = run(r, backend.clone()).await;
    assert_eq!((data.trials.len(), data.stages.len()), (8, 6));
    let seen = backend.seen.lock().unwrap();
    let second = seen.iter().find(|i| i.id.starts_with("inv2.")).unwrap();
    assert!(second.harness["AGENTS.md"].contains("compose1"));
    assert!(second.prompt.contains("artifact from") && second.prompt.contains("observed change"));
}

#[tokio::test]
async fn invalid_proposal_gets_one_repair_then_inherits_current_tree() {
    let backend = Arc::new(Scripted {
        bad_proposal: true,
        ..Default::default()
    });
    let data = run(request(), backend.clone()).await;
    assert!(data.stages.iter().all(|s| s.fallback && s.tree == tree()));
    let seen = backend.seen.lock().unwrap();
    assert_eq!(seen.iter().filter(|i| i.role == Role::General).count(), 2);
    assert!(
        seen.iter()
            .any(|i| i.prompt.contains("rejected") && i.prompt.contains("AGENTS.md"))
    );
    assert_eq!(data.receipts.len(), 12); // 4 probes + 6 invalid edits + selection + reflection
}

#[tokio::test]
async fn malformed_selector_retains_baseline() {
    let data = run(
        request(),
        Arc::new(Scripted {
            bad_selection: true,
            ..Default::default()
        }),
    )
    .await;
    assert!(data.adoption.unwrap().is_baseline);
    assert!(!data.warnings.is_empty());
}

#[tokio::test]
async fn no_successful_trial_means_no_adoption() {
    let data = run(
        request(),
        Arc::new(Scripted {
            all_fail: true,
            ..Default::default()
        }),
    )
    .await;
    assert!(data.adoption.is_none());
    assert!(!data.receipts.iter().any(|r| r.id.starts_with("reflect")));
}

#[tokio::test]
async fn reflection_requires_observed_trial_references() {
    let backend = Arc::new(Scripted {
        bad_reflection: true,
        ..Default::default()
    });
    let data = run(request(), backend.clone()).await;
    assert!(backend.saved.lock().unwrap().is_empty());
    assert!(!data.warnings.is_empty());
}

#[tokio::test]
async fn transport_retries_are_separate_budgeted_calls() {
    let backend = Arc::new(Scripted {
        transport_once: std::sync::atomic::AtomicBool::new(true),
        ..Default::default()
    });
    let data = run(request(), backend).await;
    assert_eq!(data.receipts.len(), 10);
    assert!(
        data.receipts
            .iter()
            .any(|r| r.error.as_deref() == Some("transient transport failure"))
    );
    assert!(data.stages.iter().all(|s| !s.fallback));
    assert!((data.receipts.iter().filter_map(|r| r.cost_usd).sum::<f64>() - 0.10).abs() < 1e-9);
}

#[tokio::test]
async fn replayed_checkpoint_does_not_duplicate_trials() {
    let r = request();
    let backend = Arc::new(Scripted::default());
    let wf = build_refinement_workflow(&r, backend.clone()).unwrap();
    let mut ctx = flare_workflow::WorkflowContext::new(
        WorkflowRunId::new(),
        RefinementData::new(r, "frozen".into(), tree()).unwrap(),
    );
    wf.steps[0].executor.execute(&mut ctx).await.unwrap();
    wf.steps[0].executor.execute(&mut ctx).await.unwrap();
    assert_eq!(ctx.data.trials.len(), 1);
    assert_eq!(backend.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn completed_resume_makes_no_more_calls() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("state.db");
    let r = request();
    let backend = Arc::new(Scripted::default());
    let wf = build_refinement_workflow(&r, backend.clone()).unwrap();
    let id = wf.id.clone();
    let engine = WorkflowEngine::with_store(SqliteStore::open_file(&db).unwrap());
    engine.register_workflow(wf).unwrap();
    let run = engine
        .start_workflow(
            id,
            RefinementData::new(r.clone(), "frozen".into(), tree()).unwrap(),
            String::new(),
        )
        .await
        .unwrap();
    engine
        .wait_for_completion(run, "test", Duration::from_secs(15))
        .await
        .unwrap();
    let calls = backend.seen.lock().unwrap().len();
    let reopened = WorkflowEngine::with_store(SqliteStore::open_file(&db).unwrap());
    reopened
        .register_workflow(build_refinement_workflow(&r, backend.clone()).unwrap())
        .unwrap();
    assert!(!reopened.resume_run(run).await.unwrap());
    assert_eq!(backend.seen.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn exhausted_or_cancelled_runs_do_not_adopt_or_save() {
    for cancelled in [false, true] {
        let mut r = request();
        r.limits.max_calls = 1;
        let backend = Arc::new(Scripted {
            cancelled: std::sync::atomic::AtomicBool::new(cancelled),
            ..Default::default()
        });
        let wf = build_refinement_workflow(&r, backend.clone()).unwrap();
        let id = wf.id.clone();
        let engine = WorkflowEngine::with_store(SqliteStore::open_memory().unwrap());
        engine.register_workflow(wf).unwrap();
        let run = engine
            .start_workflow(
                id,
                RefinementData::new(r, "frozen".into(), tree()).unwrap(),
                String::new(),
            )
            .await
            .unwrap();
        assert!(
            engine
                .wait_for_completion(run, "test", Duration::from_secs(15))
                .await
                .is_err()
        );
        let state = engine.get_status(run).await.unwrap();
        assert!(state.context.data.adoption.is_none());
        assert!(backend.saved.lock().unwrap().is_empty());
        assert_eq!(
            backend.seen.lock().unwrap().len(),
            if cancelled { 0 } else { 1 }
        );
    }
}

#[tokio::test]
async fn sqlite_reopen_resumes_only_unfinished_phases() {
    use flare_workflow::{StateStore, StepState, WorkflowState, WorkflowStatus};
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("state.db");
    let r = request();
    let backend = Arc::new(Scripted::default());
    let wf = build_refinement_workflow(&r, backend.clone()).unwrap();
    let run = WorkflowRunId::new();
    let data = RefinementData::new(r.clone(), "frozen".into(), tree()).unwrap();
    let mut state = WorkflowState::new(run, wf.id.clone(), data);
    state.status = WorkflowStatus::Running;
    for step in &wf.steps {
        state
            .step_states
            .insert(step.id.clone(), StepState::default());
    }
    for step in &wf.steps[..3] {
        step.executor.execute(&mut state.context).await.unwrap();
    }
    assert_eq!(backend.seen.lock().unwrap().len(), 5);
    let store = SqliteStore::open_file(&db).unwrap();
    store.save(state).await.unwrap();
    drop(store);
    let engine = WorkflowEngine::with_store(SqliteStore::open_file(&db).unwrap());
    engine.register_workflow(wf).unwrap();
    assert!(engine.resume_run(run).await.unwrap());
    engine
        .wait_for_completion(run, "resume", Duration::from_secs(15))
        .await
        .unwrap();
    assert_eq!(backend.seen.lock().unwrap().len(), 9);
    assert_eq!(
        engine
            .get_status(run)
            .await
            .unwrap()
            .context
            .data
            .trials
            .len(),
        4
    );
}
