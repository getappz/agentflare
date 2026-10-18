mod backend;
mod context;
mod receipts;
#[cfg(test)]
mod tests;
mod workspace;

use flare_workflow::{
    SqliteStore, StateStore, WorkflowEngine, WorkflowRunId, WorkflowState, WorkflowStatus,
    refinement::*,
};
use receipts::*;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use workspace::*;

type Engine = WorkflowEngine<RefinementData, SqliteStore<RefinementData>>;
pub fn default_db_path() -> PathBuf {
    crate::paths::agentflare_dir().join("refinement.db")
}
pub fn artifacts_root() -> PathBuf {
    crate::paths::agentflare_dir().join("refinement")
}

fn open_store(path: &Path) -> Result<SqliteStore<RefinementData>, String> {
    ensure_plain(path).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    SqliteStore::open_file(path).map_err(|e| e.to_string())
}

fn execution_identity(request: &RefinementRequest) -> Result<String, String> {
    let mut parts = vec![];
    for config in [&request.solver, &request.improver] {
        let agent = agent_registry::agent_by_name(&config.agent)
            .ok_or_else(|| format!("unknown agent: {}", config.agent))?;
        if agent_registry::headless_args(agent).is_none() {
            return Err(format!("{} has no headless mode", config.agent));
        }
        if request.limits.max_cost_usd.is_some() && agent != agent_registry::Agent::ClaudeCode {
            return Err("dollar ceilings require claude-code for both solver and improver; other agents support call/time limits".into());
        }
        let spec = agent_registry::spec(agent);
        let binary = agent_registry::find_binary(spec.binary_names)
            .ok_or_else(|| format!("{} is not installed", config.agent))?;
        let version = agent_registry::resolve_version(
            config.agent.as_str(),
            &binary,
            spec.version_args,
            &mut Default::default(),
        )?;
        parts.push(serde_json::json!({"agent":config.agent,"model":config.model,"binary":binary,"version":version,"launcher":identity(&std::fs::read(&binary).map_err(|e|e.to_string())?)}));
    }
    let home = dirs::home_dir().ok_or("home directory unavailable")?;
    let codex = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"));
    let files = configuration_identity(&home, &codex, &claude)?;
    let environment: std::collections::BTreeMap<_, _> = [
        "AGENTFLARE_GIT_BYPASS_AGENT",
        "AGENTFLARE_SANDBOX",
        "OPENAI_BASE_URL",
        "ANTHROPIC_BASE_URL",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "XDG_CONFIG_HOME",
        "OPENCODE_CONFIG",
    ]
    .into_iter()
    .map(|key| (key, std::env::var(key).ok().map(|v| identity(v.as_bytes()))))
    .collect();
    Ok(identity(&serde_json::to_vec(&serde_json::json!({"method":2,"executors":parts,"configuration":files,"environment":environment})).map_err(|e|e.to_string())?))
}

fn configuration_identity(home: &Path, codex: &Path, claude: &Path) -> Result<String, String> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let mut files = std::collections::BTreeMap::new();
    let mut paths = vec![
        crate::paths::agentflare_dir().join("config.toml"),
        codex.join("config.toml"),
        codex.join("AGENTS.md"),
        codex.join("AGENTS.override.md"),
        claude.join("settings.json"),
        claude.join("settings.local.json"),
        claude.join("CLAUDE.md"),
        config.join("opencode/opencode.json"),
        config.join("opencode/opencode.jsonc"),
        config.join("opencode/AGENTS.md"),
        home.join(".gemini/GEMINI.md"),
        home.join(".gemini/settings.json"),
    ];
    if let Some(path) = std::env::var_os("OPENCODE_CONFIG") {
        paths.push(path.into());
    }
    for path in paths {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e.to_string()),
        };
        files.insert(path.to_string_lossy().into_owned(), identity(&bytes));
    }
    Ok(identity(&json_bytes(&files).map_err(|e| e.to_string())?))
}

pub fn start(mut request: RefinementRequest, db: &Path) -> Result<WorkflowRunId, String> {
    request.validate().map_err(|e| e.to_string())?;
    request.repository = dunce::canonicalize(&request.repository).map_err(|e| e.to_string())?;
    let execution = execution_identity(&request)?;
    let preparation = GitControl {
        deadline_ms: now_ms()
            .saturating_add(request.limits.total_timeout_secs.saturating_mul(1000)),
        job: None,
    };
    let input = FrozenInput::capture_with_control(&request.repository, &preparation)
        .map_err(|e| e.to_string())?;
    // Read initial instructions from the frozen snapshot, not a concurrently edited checkout.
    let inspection = tempfile::tempdir().map_err(|e| e.to_string())?;
    let view = TrialWorkspace::snapshot_with_control(
        &input,
        inspection.path(),
        "initial-harness",
        preparation,
    )
    .map_err(|e| e.to_string())?;
    let base_result = read_tree(view.path());
    view.cleanup().map_err(|e| e.to_string())?;
    let base = base_result.map_err(|e| e.to_string())?;
    let identity=identity(&json_bytes(&serde_json::json!({"request":request,"snapshot":input.snapshot,"base":base,"execution":execution})).map_err(|e|e.to_string())?);
    let mut data = RefinementData::new(request, identity, base).map_err(|e| e.to_string())?;
    data.runtime
        .insert("snapshot".into(), input.snapshot.clone());
    data.runtime.insert("execution_identity".into(), execution);
    let backend = Arc::new(backend::NativeBackend {
        input,
        db: db.to_path_buf(),
        root: artifacts_root(),
        concurrency: Arc::new(tokio::sync::Semaphore::new(4)),
    });
    start_with_backend(data, db, backend)
}

pub(crate) fn start_with_backend(
    data: RefinementData,
    db: &Path,
    backend: Arc<dyn RefinementBackend>,
) -> Result<WorkflowRunId, String> {
    let wf = build_refinement_workflow(&data.request, backend).map_err(|e| e.to_string())?;
    let id = wf.id.clone();
    let engine = Engine::with_store(open_store(db)?)
        .with_runtime_handle(crate::workflow::blocking_runtime_handle());
    engine.register_workflow(wf).map_err(|e| e.to_string())?;
    crate::workflow::blocking_runtime()
        .block_on(engine.start_workflow(id, data, String::new()))
        .map_err(|e| e.to_string())
}
pub fn status(run: WorkflowRunId, db: &Path) -> Result<WorkflowState<RefinementData>, String> {
    let store = open_store(db)?;
    crate::workflow::blocking_runtime()
        .block_on(store.load(run))
        .map_err(|e| e.to_string())
}

pub fn resume(run: WorkflowRunId, db: &Path) -> Result<bool, String> {
    let state = status(run, db)?;
    let data = &state.context.data;
    let expected = data
        .runtime
        .get("execution_identity")
        .ok_or("missing executor identity")?;
    if &execution_identity(&data.request)? != expected {
        return Err(
            "executor version, model configuration or policy changed; start a new refinement run"
                .into(),
        );
    }
    let root = artifacts_root().join(run.to_string());
    data.request.validate().map_err(|e| e.to_string())?;
    let snapshot = data
        .runtime
        .get("snapshot")
        .ok_or("missing frozen snapshot")?;
    let actual=identity(&json_bytes(&serde_json::json!({"request":data.request,"snapshot":snapshot,"base":data.base,"execution":expected})).map_err(|e|e.to_string())?);
    if actual != data.identity {
        return Err("saved task, limits, snapshot, or initial harness changed".into());
    }
    if !root.join("ledger.json").is_file() {
        return Err("missing invocation ledger; cannot safely reconstruct consumed budget".into());
    }
    RunLedger::open(&root, &data.identity, &data.request.limits)
        .and_then(|l| l.verify())
        .map_err(|e| e.to_string())?;
    if state.status.is_terminal() {
        if state.status == WorkflowStatus::Completed {
            export(run, data.clone(), &artifacts_root())?;
        }
        return Ok(false);
    }
    let snapshot = data
        .runtime
        .get("snapshot")
        .ok_or("missing frozen snapshot")?
        .clone();
    let input = FrozenInput {
        repository: data.request.repository.clone(),
        snapshot,
    };
    let backend = Arc::new(backend::NativeBackend {
        input,
        db: db.to_path_buf(),
        root: artifacts_root(),
        concurrency: Arc::new(tokio::sync::Semaphore::new(4)),
    });
    let wf = build_refinement_workflow(&data.request, backend).map_err(|e| e.to_string())?;
    let engine = Engine::with_store(open_store(db)?)
        .with_runtime_handle(crate::workflow::blocking_runtime_handle());
    engine.register_workflow(wf).map_err(|e| e.to_string())?;
    crate::workflow::blocking_runtime()
        .block_on(engine.resume_run(run))
        .map_err(|e| e.to_string())
}

pub fn wait(run: WorkflowRunId, db: &Path) -> Result<RefinementReport, String> {
    wait_with_root(run, db, &artifacts_root())
}

pub fn cancel(run: WorkflowRunId, db: &Path) -> Result<(), String> {
    let store = open_store(db)?;
    let engine = Engine::with_store(store.clone())
        .with_runtime_handle(crate::workflow::blocking_runtime_handle());
    crate::workflow::blocking_runtime().block_on(async {
        engine
            .cancel_workflow(run)
            .await
            .map_err(|e| e.to_string())?;
        store
            .update(run, |s| {
                if s.status == WorkflowStatus::Cancelled {
                    s.context.data.adoption = None;
                }
            })
            .await
            .map_err(|e| e.to_string())
    })
}
pub(crate) fn wait_with_root(
    run: WorkflowRunId,
    db: &Path,
    artifacts: &Path,
) -> Result<RefinementReport, String> {
    let store = open_store(db)?;
    let state = crate::workflow::blocking_runtime().block_on(async {
        loop {
            let state = store.load(run).await.map_err(|e| e.to_string())?;
            if state.status.is_terminal() {
                return Ok::<_, String>(state);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })?;
    if state.status == WorkflowStatus::Completed {
        return export(run, state.context.data, artifacts);
    }
    let error = state
        .error
        .unwrap_or_else(|| format!("refinement ended: {:?}", state.status));
    let mut data = state.context.data;
    data.adoption = None;
    data.warnings.push(error.clone());
    if artifacts
        .join(run.to_string())
        .join("ledger.json")
        .is_file()
    {
        export(run, data, artifacts)?;
    }
    Err(error)
}

fn export(
    run: WorkflowRunId,
    mut data: RefinementData,
    artifacts: &Path,
) -> Result<RefinementReport, String> {
    let root = artifacts.join(run.to_string());
    if !root.join("ledger.json").is_file() {
        return Err("missing invocation ledger; refusing to reset consumed budget".into());
    }
    let ledger =
        RunLedger::open(&root, &data.identity, &data.request.limits).map_err(|e| e.to_string())?;
    ledger.verify().map_err(|e| e.to_string())?;
    let lock_path = root.join("export.lock");
    ensure_plain(&lock_path).map_err(|e| e.to_string())?;
    let export_lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| e.to_string())?;
    fs2::FileExt::lock_exclusive(&export_lock).map_err(|e| e.to_string())?;
    data.budget = serde_json::to_value(ledger.summary().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if let Some(adoption) = &data.adoption {
        let trial = data
            .trials
            .iter()
            .find(|t| t.label == adoption.trial_id)
            .ok_or("adopted trial is missing")?;
        if !trial.receipt.successful() || harness_identity(&trial.tree) != adoption.harness_identity
        {
            return Err("adopted harness identity changed".into());
        }
        let dest = root.join("adopted");
        ensure_plain(&dest).map_err(|e| e.to_string())?;
        if dest.exists() {
            if harness_identity(&read_tree(&dest).map_err(|e| e.to_string())?)
                != adoption.harness_identity
            {
                return Err("exported adopted harness changed".into());
            }
        } else {
            let staging = tempfile::Builder::new()
                .prefix(".adopted-")
                .tempdir_in(&root)
                .map_err(|e| e.to_string())?;
            stage_tree(staging.path(), &trial.tree).map_err(|e| e.to_string())?;
            std::fs::rename(staging.path(), &dest).map_err(|e| e.to_string())?;
        }
    }
    write_json(&root.join("report.json"), &data).map_err(|e| e.to_string())?;
    Ok(data)
}
