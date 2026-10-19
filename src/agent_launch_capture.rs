/// `run_captured_for_job` with an explicit [`StallConfig`] (tests pass
/// short windows instead of mutating the process env).
pub(crate) fn run_captured_with_stall(
    cmd: Command,
    hard_cap: Duration,
    idle_timeout: Duration,
    stdin: Option<&str>,
    cancel_job: Option<&str>,
    stall: StallConfig,
) -> std::io::Result<Captured> {
    run_captured_with_completion(cmd, hard_cap, idle_timeout, stdin, cancel_job, stall, false)
}

// Claude may finish its response before background tools release the process.
fn claude_json_output(args: &[String]) -> bool {
    args.iter()
        .enumerate()
        .filter_map(|(i, arg)| {
            if arg == "--output-format" {
                args.get(i + 1).map(String::as_str)
            } else {
                arg.strip_prefix("--output-format=")
            }
        })
        .next_back()
        .is_some_and(|format| matches!(format, "json" | "stream-json"))
}

fn claude_result(bytes: &[u8]) -> Option<serde_json::Value> {
    if bytes.trim_ascii_end().last() != Some(&b'}') {
        return None;
    }
    let event: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    (event.get("type")?.as_str()? == "result"
        && event.get("subtype")?.is_string()
        && event.get("is_error")?.is_boolean()
        && (event
            .get("result")
            .is_some_and(serde_json::Value::is_string)
            || event.get("is_error")?.as_bool()?))
    .then_some(event)
}

fn run_captured_with_completion(
    mut cmd: Command,
    hard_cap: Duration,
    idle_timeout: Duration,
    stdin: Option<&str>,
    cancel_job: Option<&str>,
    stall: StallConfig,
    detect_claude_result: bool,
) -> std::io::Result<Captured> {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    // Make the child the leader of a new process group so any descendants it
    // spawns (which inherit the group by default) can be killed together via
    // `kill_tree` — see its doc comment for why this matters.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    // NOTE: `CREATE_NO_WINDOW` is deliberately NOT applied here. It breaks
    // script-shim agents (`.cmd`/`.bat`/`.ps1`, which chain through
    // cmd.exe → powershell.exe and need a real console): cursor-agent's
    // `.cmd` shim hangs with zero output under `CREATE_NO_WINDOW`, whereas a
    // native `.exe` hides cleanly. The decision is per-agent, made by
    // `run_headless` (the only production caller) from the resolved binary's
    // extension — see its `#[cfg(windows)] creation_flags` block.
    let record_dir = cmd
        .get_current_dir()
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok());
    let mut child = cmd.spawn()?;
    // Removed again when this function returns (child reaped); left behind
    // only if this whole process dies first -- exactly the case
    // `orphan_reconcile` needs it for.
    let _pid_record = record_dir
        .as_deref()
        .and_then(|dir| AgentPidRecord::write(dir, child.id()));

    if let Some(text) = stdin {
        let mut pipe = child.stdin.take().expect("stdin piped above");
        let text = text.to_owned();
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = pipe.write_all(text.as_bytes());
            // `pipe` drops here, closing the write end so the child sees EOF.
        });
    }

    let activity = Arc::new(AtomicU64::new(0));
    let stop_readers = Arc::new(AtomicBool::new(false));
    let output_progress = Arc::new(std::sync::Mutex::new(OutputProgress::new(Instant::now())));
    let terminal = Arc::new(std::sync::Mutex::new(None::<(Instant, serde_json::Value)>));

    let mut pipe = child.stdout.take().expect("stdout piped above");
    let stdout_activity = activity.clone();
    let stdout_progress = output_progress.clone();
    let stdout_stop = stop_readers.clone();
    let stdout_terminal = terminal.clone();
    let reader = std::thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        let mut line_start = 0;
        let mut chunk = [0u8; 8192];
        // Read in chunks (rather than `read_to_string` straight to EOF) so
        // `activity` reflects bytes as they arrive, not only once the child
        // exits — the wait loop below needs that to detect a stalled child
        // before it closes its pipes.
        while !stdout_stop.load(Ordering::Relaxed) {
            let capacity = match available_output_bytes(&pipe, chunk.len()) {
                Ok(Some(capacity)) => capacity,
                Ok(None) => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            match pipe.read(&mut chunk[..capacity]) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if detect_claude_result {
                        let mut result = stdout_terminal.lock().unwrap_or_else(|e| e.into_inner());
                        if result.is_none() {
                            // Parse raw bytes so split UTF-8 and large result lines stay intact.
                            while let Some(end) = buf[line_start..].iter().position(|b| *b == b'\n')
                            {
                                let end = line_start + end + 1;
                                if let Some(event) = claude_result(&buf[line_start..end]) {
                                    *result = Some((Instant::now(), event));
                                    break;
                                }
                                line_start = end;
                            }
                            if result.is_none()
                                && let Some(event) = claude_result(&buf[line_start..])
                            {
                                *result = Some((Instant::now(), event));
                            }
                        }
                    }
                    stdout_activity.fetch_add(n as u64, Ordering::Relaxed);
                    if let Ok(mut p) = stdout_progress.lock() {
                        p.feed(0, &String::from_utf8_lossy(&chunk[..n]), Instant::now());
                    }
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    });
    let mut err_pipe = child.stderr.take().expect("stderr piped above");
    let stderr_activity = activity.clone();
    let stderr_progress = output_progress.clone();
    let stderr_stop = stop_readers.clone();
    let err_reader = std::thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        while !stderr_stop.load(Ordering::Relaxed) {
            let capacity = match available_output_bytes(&err_pipe, chunk.len()) {
                Ok(Some(capacity)) => capacity,
                Ok(None) => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            match err_pipe.read(&mut chunk[..capacity]) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    stderr_activity.fetch_add(n as u64, Ordering::Relaxed);
                    if let Ok(mut p) = stderr_progress.lock() {
                        p.feed(1, &String::from_utf8_lossy(&chunk[..n]), Instant::now());
                    }
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    });

    let start = Instant::now();
    let mut last_activity_bytes = 0u64;
    let mut last_activity_at = Instant::now();
    let mut timed_out = false;
    let mut idle_killed = false;
    let mut cancelled = false;
    let mut last_cancel_poll = Instant::now();
    let mut stalled = false;
    let mut exhausted: Option<String> = None;
    let mut last_progress_poll = Instant::now();
    // Fingerprinted lazily (first poll), so a short-lived child never pays
    // for a `git status` at all.
    let mut worktree: Option<WorktreeProgress> = None;
    let mut completed_cleanup = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let bytes_so_far = activity.load(Ordering::Relaxed);
        if bytes_so_far != last_activity_bytes {
            last_activity_bytes = bytes_so_far;
            last_activity_at = Instant::now();
        }
        if start.elapsed() >= hard_cap {
            kill_tree(&mut child);
            let status = child.wait()?;
            timed_out = true;
            break status;
        }
        let completed_at = terminal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(at, _)| *at);
        let completion_ready =
            completed_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(1));
        if completed_at.is_none() && last_activity_at.elapsed() >= idle_timeout {
            kill_tree(&mut child);
            let status = child.wait()?;
            timed_out = true;
            idle_killed = true;
            break status;
        }
        // Hung-but-chatty detection (see `agent_launch_progress.rs`),
        // evaluated ~1/s; the worktree itself is only fingerprinted every
        // `stall.check_every`.
        if completed_at.is_none() && last_progress_poll.elapsed() >= Duration::from_secs(1) {
            last_progress_poll = Instant::now();
            let now = Instant::now();
            let (repetitive, last_novel, exhaustion_seen, exhausted_line) = {
                let p = output_progress.lock().unwrap_or_else(|e| e.into_inner());
                (
                    p.is_repetitive(),
                    p.last_novel_at(),
                    p.exhaustion_first_seen(),
                    p.exhausted().map(str::to_string),
                )
            };
            let wt =
                worktree.get_or_insert_with(|| WorktreeProgress::new(record_dir.clone(), start));
            if !stall.window.is_zero() && start.elapsed() >= stall.check_every {
                wt.poll(now, stall.check_every);
            }
            if stall.exhaustion_kill && exhaustion_seen {
                wt.note_exhaustion_seen();
            }
            let stop = if let Some(line) =
                exhausted_line.filter(|_| stall.exhaustion_kill && wt.unchanged_since_exhaustion())
            {
                exhausted = Some(line);
                true
            } else {
                is_stalled(now, wt.last_change, last_novel, repetitive, stall.window)
            };
            if stop {
                kill_tree(&mut child);
                let status = child.wait()?;
                stalled = exhausted.is_none();
                break status;
            }
        }
        // A cancelled job (item reassigned to another agent) must not keep its
        // agent CLI running. Polled ~1/s -- it costs a DB read.
        if let Some(job_id) = cancel_job
            && (last_cancel_poll.elapsed() >= Duration::from_secs(1) || completion_ready)
        {
            last_cancel_poll = Instant::now();
            if agentflare_jobs::cancel::job_cancelled(job_id) {
                kill_tree(&mut child);
                let status = child.wait()?;
                cancelled = true;
                break status;
            }
        }
        if completion_ready {
            // A real nonzero exit must win over the cleanup path.
            if let Some(status) = child.try_wait()? {
                break status;
            }
            kill_tree(&mut child);
            completed_cleanup = true;
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    // Helpers can inherit the pipes after their agent exits; EOF must not defeat timeouts.
    let drain_started = Instant::now();
    while !(reader.is_finished() && err_reader.is_finished())
        && drain_started.elapsed() < Duration::from_secs(1)
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    stop_readers.store(true, Ordering::Relaxed);
    let stdout = reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    let killed_early = stalled || exhausted.is_some();
    Ok(Captured {
        success: (status.success() || completed_cleanup)
            && !timed_out
            && !cancelled
            && !killed_early,
        stdout,
        stderr,
        timed_out,
        idle_killed,
        cancelled,
        stalled,
        exhausted,
        terminal_result: terminal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .map(|(_, event)| event),
    })
}
