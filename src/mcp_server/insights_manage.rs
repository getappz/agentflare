//! Management actions reuse the insights library and the proposal CLI's write safeguards.

use super::insights::{envelope, internal, invalid, redact_value};
use super::resume::resume_insights_db;
use super::types::InsightsRequest;
use flare_insights::{InsightsConfig, InsightsStore};
use rmcp::model::ErrorData;
use serde_json::{Value, json};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_LOG_BYTES: usize = 400_000;

#[derive(Default)]
struct CapturedLog {
    lines: Vec<Vec<u8>>,
    bytes: usize,
    truncated: bool,
}
impl CapturedLog {
    fn push(&mut self, line: Vec<u8>) {
        // Keep a complete prefix: evicting a credential's header can expose its body.
        if self.truncated {
            return;
        }
        if self.bytes + line.len() > MAX_LOG_BYTES {
            self.truncated = true;
            return;
        }
        self.bytes += line.len();
        self.lines.push(line);
    }
}

fn capture(
    mut reader: impl Read + Send + 'static,
    log: Arc<Mutex<CapturedLog>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buffer = [0; 4096];
        let mut line = Vec::new();
        let mut skipping = false;
        while let Ok(n) = reader.read(&mut buffer) {
            if n == 0 {
                break;
            }
            for part in buffer[..n].split_inclusive(|b| *b == b'\n') {
                if !skipping {
                    if line.len() + part.len() > MAX_LOG_BYTES {
                        line.clear();
                        skipping = true;
                        log.lock().unwrap().truncated = true;
                    } else {
                        line.extend_from_slice(part);
                    }
                }
                if part.ends_with(b"\n") {
                    if !skipping {
                        log.lock().unwrap().push(std::mem::take(&mut line));
                    }
                    skipping = false;
                }
            }
        }
        if !skipping && !line.is_empty() {
            log.lock().unwrap().push(line);
        }
    })
}

fn clip(text: &str, req: &InsightsRequest) -> Value {
    let limit = req.max_chars.unwrap_or(12_000).min(100_000);
    json!({"text":text.chars().take(limit).collect::<String>(),
        "omitted_chars":text.chars().count().saturating_sub(limit)})
}

pub(super) fn document(
    store: &InsightsStore,
    req: &InsightsRequest,
    source: Option<&str>,
    project: Option<&str>,
) -> Result<Value, ErrorData> {
    if req.action.trim() == "handoff" {
        let id = req
            .session_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| invalid("session_id is required for handoff"))?;
        let session = store
            .sessions_in_window(None, source, project, Some(id))
            .map_err(internal)?
            .into_iter()
            .next()
            .ok_or_else(|| invalid("session not found in the selected scope"))?;
        let turns = store.get_turns(&session.id).map_err(internal)?;
        use flare_insights::handoff::Verbosity;
        let verbosity = match req.verbosity.as_deref().unwrap_or("standard") {
            "minimal" => Verbosity::Minimal,
            "standard" => Verbosity::Standard,
            "verbose" => Verbosity::Verbose,
            "full" => Verbosity::Full,
            _ => return Err(invalid("verbosity must be minimal|standard|verbose|full")),
        };
        let doc = flare_insights::handoff::handoff_doc(
            &session,
            &turns,
            req.target.as_deref().unwrap_or("codex"),
            verbosity,
        );
        // Redact before clipping so a credential cannot be cut into an unrecognizable prefix.
        let mut doc = json!(doc);
        redact_value(&mut doc);
        return Ok(clip(doc.as_str().unwrap(), req));
    }
    use flare_insights::export::ExportFormat;
    let format = match req.format.as_deref().unwrap_or("json") {
        "json" => ExportFormat::Json,
        "jsonl" => ExportFormat::Jsonl,
        "html" => ExportFormat::Html,
        "deepeval" => ExportFormat::Deepeval,
        "openai" | "openai-evals" => ExportFormat::OpenAiEvals,
        _ => {
            return Err(invalid(
                "format must be json|jsonl|html|deepeval|openai|openai-evals",
            ));
        }
    };
    let limit = req.limit.unwrap_or(20).clamp(1, 100);
    let offset = req.offset.unwrap_or(0);
    let mut sessions = store
        .list_sessions_filtered(source, project, limit + 1, offset)
        .map_err(internal)?;
    let has_more = sessions.len() > limit;
    sessions.truncate(limit);
    if let Some(id) = &req.session_id {
        // Explicit session selection must not depend on where it falls in list pagination.
        sessions = store
            .sessions_in_window(None, source, project, Some(id))
            .map_err(internal)?;
        if sessions.is_empty() {
            return Err(invalid("session not found in the selected scope"));
        }
    }
    let mut turns = Vec::new();
    for s in &sessions {
        turns.extend(store.get_turns(&s.id).map_err(internal)?);
    }
    let mut bundle = json!([sessions, turns]);
    redact_value(&mut bundle);
    let (sessions, turns): (
        Vec<flare_insights::model::Session>,
        Vec<flare_insights::model::Turn>,
    ) = serde_json::from_value(bundle).map_err(internal)?;
    let content = flare_insights::export::export_sessions(&sessions, &turns, format);
    let mut result = clip(&content, req);
    result["next_offset"] =
        json!((has_more && req.session_id.is_none()).then(|| offset.saturating_add(limit)));
    if let Some(output) = &req.output {
        result["output"] = json!(output);
        result["written"] = json!(false);
        if req.yes {
            let mut file = File::options()
                .write(true)
                .create_new(true)
                .open(output)
                .map_err(internal)?;
            file.write_all(content.as_bytes()).map_err(internal)?;
            result["written"] = json!(true);
        }
    }
    Ok(result)
}

fn sync(db: &Path, req: &InsightsRequest) -> Result<Value, ErrorData> {
    if req.source.is_some() {
        return Err(invalid("sync scans all sources; use project to scope it"));
    }
    if req.prune_days.is_some_and(|days| days > 0)
        && (!req.yes || req.project.as_deref() != Some("*"))
    {
        return Err(invalid(
            "pruning requires yes=true and project=* because retention applies to the whole index",
        ));
    }
    let cwd = std::env::current_dir().map_err(internal)?;
    let mut config = InsightsConfig {
        db_path: db.into(),
        ..Default::default()
    };
    config.project = match req.project.as_deref() {
        Some("*") => None,
        Some(p) if !p.trim().is_empty() => Some(p.into()),
        _ => Some(flare_git_core::branch::repo_toplevel(&cwd).unwrap_or(cwd)),
    };
    let store = InsightsStore::open(db).map_err(internal)?;
    let empty = req
        .force
        .then(InsightsStore::open_in_memory)
        .transpose()
        .map_err(internal)?;
    let mut results = Vec::new();
    let mut ok = true;
    for (source, result) in flare_insights::ingest::IngestManager::new().scan_all(
        &config,
        empty.as_ref().unwrap_or(&store),
        |_, _, _| {},
    ) {
        match result.map_err(|e|e.to_string()).and_then(|bundle| store.commit_ingest(&config.cursor_source(&source), &bundle).map_err(|e|e.to_string())) {
            Ok(stats) => results.push(json!({"source":source,"sessions":stats.sessions,"turns":stats.turns,"tools":stats.tool_calls,"files":stats.file_events,"subagents":stats.subagents})),
            Err(error) => {ok = false; results.push(json!({"source":source,"error":error.to_string()}));}
        }
    }
    let pruned = match req.prune_days.filter(|d| *d > 0) {
        Some(days) => store
            .prune_older_than(chrono::Utc::now() - chrono::Duration::days(i64::from(days)))
            .map_err(internal)?,
        None => 0,
    };
    Ok(
        json!({"ok":ok,"sources":results,"pruned_sessions":pruned,"project":config.project,"db_path":db}),
    )
}

pub(super) fn reflect_args(req: &InsightsRequest) -> Result<Vec<String>, ErrorData> {
    if req.project.as_deref() != Some("*") || req.source.is_some() {
        return Err(invalid(
            "reflect currently uses the global ledger; pass project=* and omit source",
        ));
    }
    let op = req.operation.as_deref().unwrap_or("list");
    let mut args = vec!["reflect".into(), op.into()];
    match op {
        "list" => {
            if let Some(status) = &req.status {
                if flare_insights::proposals::ProposalStatus::parse(status).is_none() {
                    return Err(invalid("invalid proposal status"));
                }
                args.extend(["--status".into(), status.clone()]);
            }
        }
        "scan" => args.extend(["--days".into(), req.since_days.unwrap_or(30).to_string()]),
        "show" | "accept" | "reject" | "snooze" | "apply" => {
            let id = req
                .proposal_id
                .filter(|id| *id > 0)
                .ok_or_else(|| invalid("positive proposal_id is required"))?;
            if matches!(op, "accept" | "reject" | "snooze") && !req.yes {
                return Err(invalid("proposal decisions require yes=true"));
            }
            args.push(id.to_string());
            if op == "snooze" {
                let days = req.since_days.unwrap_or(30);
                if !(1..=36_500).contains(&days) {
                    return Err(invalid("snooze days must be 1..36500"));
                }
                args.extend(["--days".into(), days.to_string()]);
            }
            if op == "apply" {
                if req.yes {
                    args.push("--yes".into());
                }
                if req.global {
                    args.push("--global".into());
                }
            }
        }
        _ => {
            return Err(invalid(
                "reflect operation must be scan|list|show|accept|reject|snooze|apply",
            ));
        }
    }
    Ok(args)
}

pub(super) struct Service {
    child: Child,
    log: Arc<Mutex<CapturedLog>>,
    readers: Vec<std::thread::JoinHandle<()>>,
}
impl Service {
    fn finish_output(&mut self) {
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.finish_output();
    }
}
fn spawn(db: &Path, args: &[String]) -> Result<Service, ErrorData> {
    let log = Arc::new(Mutex::new(CapturedLog::default()));
    let mut command = flare_process::command(std::env::current_exe().map_err(internal)?);
    command
        .arg("insights")
        .arg("--db")
        .arg(db)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = command.spawn().map_err(internal)?;
    let readers = vec![
        capture(child.stdout.take().unwrap(), log.clone()),
        capture(child.stderr.take().unwrap(), log.clone()),
    ];
    Ok(Service {
        child,
        log,
        readers,
    })
}
fn output(service: &mut Service, req: &InsightsRequest) -> Result<Value, ErrorData> {
    if service.child.try_wait().map_err(internal)?.is_some() {
        service.finish_output();
    }
    let log = service.log.lock().map_err(internal)?;
    let bytes: Vec<_> = log.lines.iter().flatten().copied().collect();
    let mut text = json!(String::from_utf8_lossy(&bytes));
    redact_value(&mut text);
    let mut result = clip(text.as_str().unwrap(), req);
    result["log_truncated"] = json!(log.truncated);
    Ok(result)
}

impl super::AgentflareMcp {
    pub(super) fn insights_manage(
        services: &std::sync::Mutex<std::collections::BTreeMap<String, Service>>,
        req: &InsightsRequest,
    ) -> Result<String, ErrorData> {
        let db = resume_insights_db();
        let action = req.action.trim();
        let data = match action {
            "doctor" => {
                let config = InsightsConfig::default();
                let status = InsightsStore::open_read_only(&db)
                    .and_then(|store| store.list_sessions_filtered(None, None, 1, 0).map(|_| ()));
                let sources: Vec<_> = flare_insights::capability::capabilities().iter().map(|c|json!({"name":c.source,"present":config.sources.get(c.source.as_ref()).is_some_and(|p|p.exists())})).collect();
                json!({"db_path":db,"db_ok":status.is_ok(),"error":status.err().map(|e|e.to_string()),"sources":sources,"capabilities":flare_insights::capability::capabilities()})
            }
            "sync" => sync(&db, req)?,
            "reflect" => {
                let args = reflect_args(req)?;
                let mut service = spawn(&db, &args)?;
                let start = Instant::now();
                let status = loop {
                    if let Some(status) = service.child.try_wait().map_err(internal)? {
                        break status;
                    }
                    if start.elapsed() > Duration::from_secs(120) {
                        return Err(internal("reflect timed out after 120 seconds"));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                };
                let mut data = output(&mut service, req)?;
                data["ok"] = json!(status.success());
                data["exit_code"] = json!(status.code());
                data
            }
            "watch" | "serve" => {
                let op = req.operation.as_deref().unwrap_or("start");
                let mut services = services.lock().map_err(internal)?;
                match op {
                    "start" => {
                        if req.project.as_deref() != Some("*") || req.source.is_some() {
                            return Err(invalid(
                                "watch/serve background ingestion is global; pass project=* and omit source",
                            ));
                        }
                        if let Some(service) = services.get_mut(action)
                            && service.child.try_wait().map_err(internal)?.is_none()
                        {
                            return envelope(
                                action,
                                json!({"running":true,"pid":service.child.id(),"already_running":true}),
                            );
                        }
                        let args = if action == "serve" {
                            let port = req.port.unwrap_or(3456);
                            if port == 0 {
                                return Err(invalid("port must be 1..65535"));
                            }
                            vec!["serve".into(), "--port".into(), port.to_string()]
                        } else {
                            let interval = req.interval_secs.unwrap_or(5);
                            if interval == 0 {
                                return Err(invalid("interval_secs must be at least 1"));
                            }
                            vec![
                                "watch".into(),
                                "--interval-secs".into(),
                                interval.to_string(),
                            ]
                        };
                        let service = spawn(&db, &args)?;
                        let pid = service.child.id();
                        services.insert(action.into(), service);
                        json!({"started":true,"pid":pid,"scope":"global","url":(action=="serve").then(||format!("http://127.0.0.1:{}",req.port.unwrap_or(3456))),"note":"Use operation=status to check readiness; stop on MCP disconnect."})
                    }
                    "status" => match services.get_mut(action) {
                        Some(service) => {
                            let status = service.child.try_wait().map_err(internal)?;
                            let mut data = output(service, req)?;
                            data["running"] = json!(status.is_none());
                            data["pid"] = json!(service.child.id());
                            data["exit_code"] = json!(status.and_then(|s| s.code()));
                            data
                        }
                        None => json!({"running":false}),
                    },
                    "stop" => {
                        let stopped = services.remove(action).is_some();
                        json!({"running":false,"stopped":stopped})
                    }
                    _ => return Err(invalid("watch/serve operation must be start|status|stop")),
                }
            }
            _ => unreachable!(),
        };
        envelope(action, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_logs_retain_a_complete_prefix_with_a_fixed_memory_bound() {
        let log = Arc::new(Mutex::new(CapturedLog::default()));
        let mut bytes = Vec::new();
        for _ in 0..1000 {
            bytes.extend_from_slice(b"older line with fixture data\n");
        }
        // An overlong line is dropped whole, preserving redaction boundaries.
        bytes.extend(std::iter::repeat_n(b'x', MAX_LOG_BYTES + 1));
        bytes.extend_from_slice(b"\nrecent diagnostic\n");
        capture(std::io::Cursor::new(bytes), log.clone())
            .join()
            .unwrap();
        let log = log.lock().unwrap();
        assert!(log.bytes <= MAX_LOG_BYTES);
        assert!(log.truncated);
        assert!(!log.lines.iter().any(|line| line == b"recent diagnostic\n"));
        assert!(log.lines.iter().all(|line| line.len() < MAX_LOG_BYTES));
    }

    #[test]
    fn child_logs_preserve_multiline_redaction_boundaries() {
        let mut log = CapturedLog::default();
        log.push(format!("-----BEGIN {}-----\n", "PRIVATE KEY").into_bytes());
        for _ in 0..10 {
            log.push(b"sensitive-body-fixture\n".repeat(MAX_LOG_BYTES / 100));
        }
        assert!(log.bytes <= MAX_LOG_BYTES);
        assert!(log.truncated);
        let bytes: Vec<_> = log.lines.iter().flatten().copied().collect();
        let redacted = crate::mcp_server::secret_scan::redact(&String::from_utf8_lossy(&bytes));
        assert!(!redacted.contains("sensitive-body-fixture"));
        assert!(redacted.contains("[REDACTED]"));
    }
}
