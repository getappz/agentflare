use super::receipts::*;
use flare_workflow::{WorkflowRunId, refinement::*};

fn invocation(id: &str) -> Invocation {
    Invocation {
        id: id.into(),
        run_id: WorkflowRunId::new(),
        identity: "frozen".into(),
        role: Role::Probe,
        agent: AgentConfig {
            agent: "codex".into(),
            model: "test".into(),
        },
        prompt: "task".into(),
        harness: HarnessTree::from([("AGENTS.md".into(), "work".into())]),
        checks: vec![vec!["check".into()]],
        limits: Limits::default(),
    }
}

#[test]
fn concurrent_reservations_never_exceed_limit() {
    let dir = tempfile::tempdir().unwrap();
    let limits = Limits {
        max_calls: 1,
        ..Limits::default()
    };
    let ledger = std::sync::Arc::new(RunLedger::open(dir.path(), "frozen", &limits).unwrap());
    let handles: Vec<_> = (0..2)
        .map(|i| {
            let ledger = ledger.clone();
            std::thread::spawn(move || ledger.reserve(&invocation(&format!("call{i}"))).is_ok())
        })
        .collect();
    assert_eq!(
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|ok| *ok)
            .count(),
        1
    );
}

#[test]
fn interrupted_attempt_is_charged_without_reexecution() {
    let dir = tempfile::tempdir().unwrap();
    let limits = Limits::default();
    let ledger = RunLedger::open(dir.path(), "frozen", &limits).unwrap();
    let inv = invocation("call1");
    ledger.reserve(&inv).unwrap();
    let reopened = RunLedger::open(dir.path(), "frozen", &limits).unwrap();
    assert!(matches!(
        reopened.reserve(&inv),
        Err(RefinementError::Interrupted(_))
    ));
    assert_eq!(reopened.summary().unwrap().calls, 1);
}

#[test]
fn changed_identity_rejects_existing_receipt() {
    let dir = tempfile::tempdir().unwrap();
    RunLedger::open(dir.path(), "frozen", &Limits::default()).unwrap();
    assert!(RunLedger::open(dir.path(), "changed", &Limits::default()).is_err());
}

#[test]
fn unknown_cost_keeps_reserved_charge() {
    let dir = tempfile::tempdir().unwrap();
    let limits = Limits {
        max_calls: 1,
        max_cost_usd: Some(1.0),
        ..Limits::default()
    };
    let ledger = RunLedger::open(dir.path(), "frozen", &limits).unwrap();
    let reservation = match ledger.reserve(&invocation("call1")).unwrap() {
        Reservation::Fresh(r) => r,
        _ => panic!("fresh call expected"),
    };
    ledger
        .complete(
            &reservation,
            &Receipt {
                id: "call1".into(),
                ..Receipt::default()
            },
        )
        .unwrap();
    let summary = ledger.summary().unwrap();
    assert_eq!(summary.charged_usd, Some(1.0));
    assert_eq!(summary.unknown_cost_calls, 1);
}

#[test]
fn every_trial_starts_from_same_dirty_snapshot() {
    use super::workspace::*;
    use flare_git_core::shell::run_in;
    let repo = tempfile::tempdir().unwrap();
    run_in(repo.path(), &["init"]).unwrap();
    run_in(repo.path(), &["config", "user.email", "test@example.test"]).unwrap();
    run_in(repo.path(), &["config", "user.name", "Test"]).unwrap();
    std::fs::write(repo.path().join("input.txt"), "before").unwrap();
    run_in(repo.path(), &["add", "."]).unwrap();
    run_in(repo.path(), &["commit", "-m", "base"]).unwrap();
    std::fs::write(repo.path().join("input.txt"), "dirty frozen input").unwrap();
    let before = run_in(repo.path(), &["status", "--porcelain"]).unwrap();
    let frozen = FrozenInput::capture(repo.path()).unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    let one = TrialWorkspace::create(&frozen, artifacts.path(), "one", &invocation("one").harness)
        .unwrap();
    std::fs::write(one.path().join("input.txt"), "mutated trial").unwrap();
    run_in(one.path(), &["add", "input.txt"]).unwrap();
    run_in(one.path(), &["commit", "-m", "solver change"]).unwrap();
    assert!(
        public_artifacts(one.path(), &frozen.snapshot)
            .unwrap()
            .contains("mutated trial")
    );
    let two = TrialWorkspace::create(&frozen, artifacts.path(), "two", &invocation("two").harness)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(two.path().join("input.txt")).unwrap(),
        "dirty frozen input"
    );
    assert_eq!(
        run_in(repo.path(), &["status", "--porcelain"]).unwrap(),
        before
    );
    one.cleanup().unwrap();
    two.cleanup().unwrap();
}

#[test]
fn staging_rejects_unsafe_harness_before_writes() {
    use super::workspace::stage_tree;
    let root = tempfile::tempdir().unwrap();
    let t = HarnessTree::from([("C:/outside".into(), "bad".into())]);
    assert!(stage_tree(root.path(), &t).is_err());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn staging_rejects_linked_skill_ancestor() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let link = root.path().join(".agents");
    #[cfg(windows)]
    assert!(flare_process::command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", "New-Item -ItemType Junction -Path $env:REFINEMENT_TEST_LINK -Target $env:REFINEMENT_TEST_TARGET | Out-Null"])
        .env("REFINEMENT_TEST_LINK", &link)
        .env("REFINEMENT_TEST_TARGET", outside.path())
        .status().unwrap().success());
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    assert!(super::workspace::stage_tree(root.path(), &invocation("link").harness).is_err());
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    #[cfg(windows)]
    std::fs::remove_dir(&link).unwrap();
}

#[test]
fn probe_explicitly_loads_the_staged_harness_for_every_executor() {
    let prompt = super::backend::probe_prompt("Repair the parser.");
    assert!(prompt.starts_with("Read AGENTS.md"));
    assert!(prompt.contains(".agents/skills/"));
    assert!(prompt.ends_with("Repair the parser."));
}

#[test]
fn cancellation_preserves_completed_solver_usage_and_evidence() {
    let root = tempfile::tempdir().unwrap();
    let ledger = RunLedger::open(root.path(), "frozen", &Limits::default()).unwrap();
    let inv = invocation("cancelled-check");
    let Reservation::Fresh(reservation) = ledger.reserve(&inv).unwrap() else {
        panic!("fresh")
    };
    let receipt = Receipt {
        id: inv.id.clone(),
        reply: "completed answer".into(),
        cost_usd: Some(0.25),
        input_tokens: 30,
        session_id: Some("session".into()),
        evidence: "actual changes".into(),
        accepted: Some(true),
        ..Default::default()
    };
    assert!(matches!(
        super::backend::settle_receipt(
            &ledger,
            &reservation,
            receipt,
            Err(RefinementError::Cancelled)
        ),
        Err(RefinementError::Cancelled)
    ));
    let Reservation::Cached(saved) = ledger.reserve(&inv).unwrap() else {
        panic!("completed receipt")
    };
    assert_eq!(saved.cost_usd, Some(0.25));
    assert_eq!(saved.reply, "completed answer");
    assert_eq!(saved.evidence, "actual changes");
    assert_eq!(saved.session_id.as_deref(), Some("session"));
    assert_eq!(saved.accepted, None);
    assert!(saved.error.is_some());
}

#[test]
fn global_instructions_are_part_of_execution_identity() {
    let home = tempfile::tempdir().unwrap();
    let codex = home.path().join(".codex");
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&codex).unwrap();
    std::fs::create_dir_all(&claude).unwrap();
    let before = super::configuration_identity(home.path(), &codex, &claude).unwrap();
    std::fs::write(claude.join("CLAUDE.md"), "changed global instructions").unwrap();
    let after = super::configuration_identity(home.path(), &codex, &claude).unwrap();
    assert_ne!(before, after);
    std::fs::write(codex.join("AGENTS.md"), "changed Codex instructions").unwrap();
    assert_ne!(
        after,
        super::configuration_identity(home.path(), &codex, &claude).unwrap()
    );
}

#[test]
fn checkout_hook_is_bounded_and_its_child_is_terminated() {
    use super::workspace::*;
    use flare_git_core::shell::run_in;
    let repo = tempfile::tempdir().unwrap();
    run_in(repo.path(), &["init"]).unwrap();
    run_in(repo.path(), &["config", "user.email", "test@example.test"]).unwrap();
    run_in(repo.path(), &["config", "user.name", "Test"]).unwrap();
    std::fs::write(repo.path().join("input.txt"), "input").unwrap();
    run_in(repo.path(), &["add", "."]).unwrap();
    run_in(repo.path(), &["commit", "-m", "base"]).unwrap();
    let frozen = FrozenInput::capture(repo.path()).unwrap();
    let hook = repo.path().join(".git/hooks/post-checkout");
    std::fs::write(&hook, "#!/bin/sh\nprintf started > \"$(git rev-parse --git-common-dir)/refinement-hook-started\"\nsleep 30\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let artifacts = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let result = TrialWorkspace::snapshot_with_control(
        &frozen,
        artifacts.path(),
        "hook",
        GitControl {
            deadline_ms: now_ms() + 1000,
            job: None,
        },
    );
    assert!(matches!(result, Err(RefinementError::Exhausted(_))));
    // Windows uses two taskkill tree passes; allow their latency under parallel builds.
    let cleanup_seconds = if cfg!(windows) { 15 } else { 5 };
    assert!(
        started.elapsed() < std::time::Duration::from_secs(cleanup_seconds),
        "checkout cleanup took {:?}",
        started.elapsed()
    );
    assert!(repo.path().join(".git/refinement-hook-started").exists());
}

#[test]
fn active_artifact_collection_obeys_its_run_deadline() {
    use super::workspace::*;
    let root = tempfile::tempdir().unwrap();
    let expired = GitControl {
        deadline_ms: now_ms() - 1,
        job: None,
    };
    assert!(matches!(
        public_artifacts_with_control(root.path(), "HEAD", &expired),
        Err(RefinementError::Exhausted(_))
    ));
    assert!(matches!(
        artifact_digest_with_control(root.path(), &expired),
        Err(RefinementError::Exhausted(_))
    ));
}

#[test]
fn artifact_hashing_stops_between_reads_at_its_deadline() {
    use sha2::Digest;
    struct SlowInput;
    impl std::io::Read for SlowInput {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(std::time::Duration::from_millis(10));
            buf[0] = 1;
            Ok(1)
        }
    }
    let control = super::workspace::GitControl {
        deadline_ms: now_ms() + 50,
        job: None,
    };
    let started = std::time::Instant::now();
    assert!(matches!(
        super::workspace::hash_contents(&mut SlowInput, &mut sha2::Sha256::new(), &control),
        Err(RefinementError::Exhausted(_))
    ));
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}

#[test]
fn acceptance_argv_does_not_invoke_a_shell() {
    let root = tempfile::tempdir().unwrap();
    let argv = vec![
        "rustc".into(),
        "--version".into(),
        ">".into(),
        "owned".into(),
    ];
    let result =
        super::backend::run_check(root.path(), &argv, std::time::Duration::from_secs(10), None)
            .unwrap();
    assert!(result.stdout.contains("rustc "));
    assert!(!root.path().join("owned").exists());
}

#[test]
fn experiences_do_not_cross_project_or_domain() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::memory::schema::migrations()
        .to_latest(&mut conn)
        .unwrap();
    let e = Experience {
        project: "project-a".into(),
        domain: "rust".into(),
        cue: "cue".into(),
        lesson: "lesson".into(),
        evidence: "evidence".into(),
        trial_ids: vec!["base1".into()],
        confidence: "observed".into(),
    };
    let run = WorkflowRunId::new();
    super::backend::save_experience_to(&conn, run, &e).unwrap();
    super::backend::save_experience_to(&conn, run, &e).unwrap();
    assert_eq!(
        super::backend::load_experiences_from(&conn, "project-a", "rust")
            .unwrap()
            .len(),
        1
    );
    assert!(
        super::backend::load_experiences_from(&conn, "project-a", "legal")
            .unwrap()
            .is_empty()
    );
    assert!(
        super::backend::load_experiences_from(&conn, "project-b", "rust")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn refine_cli_parses_start_status_resume() {
    use clap::Parser;
    for args in [
        vec!["agentflare", "workflow", "refine", "request.json"],
        vec![
            "agentflare",
            "workflow",
            "refine-status",
            "019e0000-0000-7000-8000-000000000000",
        ],
        vec![
            "agentflare",
            "workflow",
            "refine-resume",
            "019e0000-0000-7000-8000-000000000000",
        ],
        vec![
            "agentflare",
            "workflow",
            "refine-cancel",
            "019e0000-0000-7000-8000-000000000000",
        ],
    ] {
        assert!(crate::cli::Cli::try_parse_from(args).is_ok());
    }
}

struct CountingBackend {
    root: std::path::PathBuf,
    calls: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl RefinementBackend for CountingBackend {
    async fn execute(&self, inv: Invocation) -> Result<Receipt, RefinementError> {
        let ledger = RunLedger::open(
            &self.root.join(inv.run_id.to_string()),
            &inv.identity,
            &inv.limits,
        )?;
        let reservation = match ledger.reserve(&inv)? {
            Reservation::Cached(r) => return Ok(r),
            Reservation::Fresh(r) => r,
        };
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let reply = match inv.role {
            Role::Probe => "actual fixture answer".to_string(),
            Role::Select => r#"{"choice":"abstain","reason":"baseline passes"}"#.into(),
            Role::Reflect => "null".into(),
            _ => serde_json::json!({"tree":inv.harness,"notes":"tested fixture"}).to_string(),
        };
        let r = Receipt {
            id: inv.id,
            reply,
            accepted: Some(true),
            cost_usd: Some(0.01),
            ..Receipt::default()
        };
        ledger.complete(&reservation, &r)?;
        Ok(r)
    }
    async fn load_experiences(&self, _: &str, _: &str) -> Result<Vec<Experience>, RefinementError> {
        Ok(vec![])
    }
    async fn save_experience(
        &self,
        _: WorkflowRunId,
        _: &Experience,
    ) -> Result<(), RefinementError> {
        Ok(())
    }
}

#[test]
fn full_service_exports_proposed_revision_without_models() {
    use std::sync::{Arc, atomic::Ordering};
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("refine.db");
    let artifacts = root.path().join("artifacts");
    let r:RefinementRequest=serde_json::from_value(serde_json::json!({"task_id":"task","domain":"rust","prompt":"do task","repository":root.path(),"solver":{"agent":"codex","model":"test"},"improver":{"agent":"codex","model":"test"},"mode":"parallel","k":1,"checks":[["rustc","--version"]]})).unwrap();
    let data = RefinementData::new(r, "frozen".into(), invocation("fixture").harness).unwrap();
    let backend = Arc::new(CountingBackend {
        root: artifacts.clone(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let run = super::start_with_backend(data, &db, backend.clone()).unwrap();
    let report = super::wait_with_root(run, &db, &artifacts).unwrap();
    assert!(report.adoption.unwrap().is_baseline);
    assert!(
        artifacts
            .join(run.to_string())
            .join("adopted/AGENTS.md")
            .is_file()
    );
    assert!(
        artifacts
            .join(run.to_string())
            .join("report.json")
            .is_file()
    );
    let calls = backend.calls.load(Ordering::SeqCst);
    super::wait_with_root(run, &db, &artifacts).unwrap();
    assert_eq!(backend.calls.load(Ordering::SeqCst), calls);
    assert_eq!(calls, 9); // four probes, three edits, selector, reflector
    assert!(!root.path().join("AGENTS.md").exists());
}

#[test]
fn changed_artifacts_reject_completed_receipts() {
    let root = tempfile::tempdir().unwrap();
    let trial = root.path().join("trial");
    std::fs::create_dir(&trial).unwrap();
    flare_git_core::shell::run_in(&trial, &["init"]).unwrap();
    std::fs::write(trial.join("answer.txt"), "original").unwrap();
    let ledger = RunLedger::open(root.path(), "frozen", &Limits::default()).unwrap();
    let inv = invocation("call");
    let Reservation::Fresh(res) = ledger.reserve(&inv).unwrap() else {
        panic!("fresh")
    };
    ledger
        .complete(
            &res,
            &Receipt {
                id: inv.id.clone(),
                artifact_path: Some(trial.clone()),
                artifact_identity: Some(super::workspace::artifact_digest(&trial).unwrap()),
                ..Receipt::default()
            },
        )
        .unwrap();
    assert!(matches!(
        ledger.reserve(&inv).unwrap(),
        Reservation::Cached(_)
    ));
    std::fs::write(trial.join("answer.txt"), "tampered").unwrap();
    assert!(matches!(
        ledger.reserve(&inv),
        Err(RefinementError::Changed(_))
    ));
}

#[test]
fn failed_completion_write_never_creates_a_success_receipt() {
    let root = tempfile::tempdir().unwrap();
    let ledger = RunLedger::open(root.path(), "frozen", &Limits::default()).unwrap();
    let inv = invocation("call");
    let Reservation::Fresh(res) = ledger.reserve(&inv).unwrap() else {
        panic!("fresh")
    };
    let path = root.path().join("ledger.json");
    let saved = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        ledger
            .complete(
                &res,
                &Receipt {
                    id: "call".into(),
                    ..Receipt::default()
                }
            )
            .is_err()
    );
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, saved).unwrap();
    assert!(matches!(
        ledger.reserve(&inv),
        Err(RefinementError::Interrupted(_))
    ));
}

#[test]
fn deadline_is_durable_and_stops_new_reservations() {
    let root = tempfile::tempdir().unwrap();
    let limits = Limits {
        call_timeout_secs: 1,
        total_timeout_secs: 1,
        ..Default::default()
    };
    let ledger = RunLedger::open(root.path(), "frozen", &limits).unwrap();
    let deadline = ledger.summary().unwrap().deadline_ms;
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let reopened = RunLedger::open(root.path(), "frozen", &limits).unwrap();
    assert_eq!(reopened.summary().unwrap().deadline_ms, deadline);
    assert!(matches!(
        reopened.reserve(&invocation("late")),
        Err(RefinementError::Exhausted(_))
    ));
}

#[test]
fn cancellation_reaches_acceptance_process_and_ends_it() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let stop = Arc::new(AtomicBool::new(false));
    let check = stop.clone();
    let job = format!("refinement-test-{}", WorkflowRunId::new());
    let guard = agentflare_jobs::cancel::register(&job, move || check.load(Ordering::SeqCst));
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().to_path_buf();
    #[cfg(windows)]
    let argv = vec![
        "powershell.exe".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-Command".into(),
        "Start-Sleep -Seconds 30".into(),
    ];
    #[cfg(not(windows))]
    let argv = vec!["sleep".into(), "30".into()];
    let child = std::thread::spawn(move || {
        super::backend::run_check(&cwd, &argv, std::time::Duration::from_secs(10), Some(&job))
            .unwrap()
    });
    std::thread::sleep(std::time::Duration::from_millis(150));
    stop.store(true, Ordering::SeqCst);
    let result = child.join().unwrap();
    drop(guard);
    assert!(result.cancelled);
    assert!(!result.success);
}

#[test]
fn typed_cancel_clears_adoption() {
    use flare_workflow::{SqliteStore, StateStore, WorkflowId, WorkflowState, WorkflowStatus};
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("run.db");
    let run = WorkflowRunId::new();
    let r:RefinementRequest=serde_json::from_value(serde_json::json!({"task_id":"task","domain":"rust","prompt":"task","repository":root.path(),"solver":{"agent":"codex","model":"test"},"improver":{"agent":"codex","model":"test"},"mode":"parallel","k":1,"checks":[["check"]]})).unwrap();
    let mut data = RefinementData::new(r, "frozen".into(), invocation("x").harness).unwrap();
    data.adoption = Some(Adoption {
        trial_id: "base1".into(),
        harness_identity: "test".into(),
        reason: "fixture".into(),
        is_baseline: true,
    });
    let mut state = WorkflowState::new(run, WorkflowId::new("refine"), data);
    state.status = WorkflowStatus::Running;
    let store = SqliteStore::open_file(&db).unwrap();
    crate::workflow::blocking_runtime()
        .block_on(store.save(state))
        .unwrap();
    super::cancel(run, &db).unwrap();
    let state = super::status(run, &db).unwrap();
    assert_eq!(state.status, WorkflowStatus::Cancelled);
    assert!(state.context.data.adoption.is_none());
}
