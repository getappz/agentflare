// Launch engine for `agentflare agents launch <agent> [args...]`.
// Finds the agent binary on PATH, maps --model/--mode to agent-native
// flags, and executes with pass-through args and inherited stdio.
use agent_registry::detect::find_binary;
use agent_registry::{Agent, AgentSpec, Tier, headless_args, json_output_args};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum LaunchOutcome {
    Launched,
    NotFound(String),
    UnknownAgent(String),
    Extension(String),
}

pub fn run_launch(
    registry: &[AgentSpec],
    agent: &str,
    model: Option<&str>,
    mode: Option<&str>,
    args: &[String],
) -> LaunchOutcome {
    run_launch_env(registry, agent, model, mode, args, &[], false)
}

/// Like `run_launch`, but injects `env` overrides into the child and — when
/// `via_mise` is set and mise is available — launches through `mise exec` so the
/// agent (and everything it spawns) inherits mise's tool paths. Powers
/// `agentflare run`. Falls back to a plain launch if mise isn't installed.
pub fn run_launch_env(
    registry: &[AgentSpec],
    agent: &str,
    model: Option<&str>,
    mode: Option<&str>,
    args: &[String],
    env: &[(String, String)],
    via_mise: bool,
) -> LaunchOutcome {
    let spec = match registry.iter().find(|s| s.id.as_str() == agent) {
        Some(s) => s,
        None => return LaunchOutcome::UnknownAgent(agent.to_string()),
    };

    if spec.tier != Tier::Cli {
        return LaunchOutcome::Extension(format!(
            "{agent} is an editor extension — no binary to launch"
        ));
    }

    let binary = match find_binary(spec.binary_names) {
        Some(p) => p,
        None => {
            return LaunchOutcome::NotFound(format!(
                "{} not found on PATH — install it first with: agentflare agents install {agent}",
                spec.binary_names.join(" / ")
            ));
        }
    };

    // `mise exec -- <binary> …` runs the agent inside mise's environment, so its
    // tool paths are on PATH for the agent and its child shells.
    let mise = if via_mise {
        crate::mise_install::mise_bin()
    } else {
        None
    };
    let mut cmd = match &mise {
        Some(m) => {
            let mut c = Command::new(m);
            c.arg("exec").arg("--").arg(&binary);
            c
        }
        None => Command::new(&binary),
    };
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    cmd.stdin(Stdio::inherit());
    // Item #139: strip an ambient CARGO_TARGET_DIR before it reaches the
    // launched agent (and everything the agent spawns, including `cargo`).
    // Cargo's env var always outranks the worktree's `.cargo/config.toml`
    // (see `isolate_worktree_target_dir` in worktree.rs), so without this the
    // per-worktree isolation is silently shadowed for every build the agent
    // runs. `env` overrides (e.g. from a project's `.dev.vars`) are filtered
    // so they can't reintroduce the var we just stripped.
    cmd.env_remove("CARGO_TARGET_DIR");
    for (k, v) in env {
        if k == "CARGO_TARGET_DIR" {
            continue;
        }
        cmd.env(k, v);
    }

    if let Some(m) = model {
        cmd.arg("--model").arg(m);
    }
    if let Some(m) = mode {
        cmd.arg("--mode").arg(m);
    }
    for a in args {
        cmd.arg(a);
    }

    match cmd.status() {
        Ok(s) if s.success() => LaunchOutcome::Launched,
        Ok(s) => {
            let code = s.code().unwrap_or(-1);
            std::process::exit(code);
        }
        Err(e) => LaunchOutcome::NotFound(format!("failed to launch {}: {e}", binary.display())),
    }
}

/// Captured result of a headless (non-interactive) child process.
#[allow(dead_code)]
pub struct Captured {
    /// True iff the child exited 0 and did not time out.
    pub success: bool,
    /// Everything the child wrote to stdout.
    pub stdout: String,
    /// Everything the child wrote to stderr.
    pub stderr: String,
    /// True iff the child was killed for outliving `hard_cap` or going idle
    /// for `idle_timeout` (see `idle_killed` to tell the two apart).
    pub timed_out: bool,
    /// True iff `timed_out` was caused by the idle window elapsing with no
    /// new stdout/stderr bytes, rather than `hard_cap` being reached.
    pub idle_killed: bool,
}

/// Kill `child` and everything it spawned, not just the direct process. A
/// plain `child.kill()` only signals the direct child; if that child (e.g.
/// `claude -p`, `codex exec`) has itself spawned a grandchild that inherited
/// the piped stdout fd, the grandchild can keep that pipe's write end open
/// after the direct child dies — which hangs the reader thread's
/// `read_to_string` (it blocks until every writer closes the pipe) forever,
/// defeating the timeout entirely. `run_captured` puts the child in its own
/// process group (Unix) so we can kill the whole group here.
pub(crate) fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // `kill -KILL -<pid>` packs the signal and the (negative, i.e.
        // process-group-targeting) pid into two separate `-`-prefixed argv
        // entries. Some `kill` implementations misparse the second as
        // another option rather than as the target once a signal option has
        // already been consumed. `-s SIGNAME` plus a `--` end-of-options
        // marker before the pid is the portable, unambiguous idiom.
        let _ = Command::new("kill")
            .arg("-s")
            .arg("KILL")
            .arg("--")
            .arg(format!("-{}", child.id()))
            .status();
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .status();
        // `taskkill /T` builds its kill list from a single point-in-time
        // process-tree snapshot. A grandchild spawned in the narrow window
        // between that snapshot and termination (e.g. this child hadn't yet
        // exec'd its own subprocess) can survive the call entirely
        // undetected (item #78). Windows keeps a dead process's original
        // parent-PID association around for lookups until the PID is
        // reused, so a second pass a moment later still finds and kills any
        // such straggler; it's a harmless no-op once the tree is already
        // gone.
        std::thread::sleep(Duration::from_millis(250));
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = child.kill();
    }
}

/// Run `cmd` to completion, capturing stdout, and kill the child (and its
/// whole process tree) if either `hard_cap` elapses regardless of activity
/// (a backstop against runaway output, not the primary signal — see
/// `idle_timeout`), or `idle_timeout` elapses with no new stdout/stderr
/// bytes (the primary liveness signal: a task producing steady output can
/// run all the way to `hard_cap` even if that takes hours, while a task
/// that's genuinely stuck is caught quickly). Both streams are drained on
/// separate threads so a child that fills the OS pipe buffer can't deadlock
/// the wait loop; each thread bumps a shared byte counter per chunk read so
/// the wait loop can observe activity without waiting for EOF.
///
/// `stdin`, when `Some`, is piped to the child on its own thread (written in
/// full, then the handle is dropped to send EOF) rather than passed as an
/// argv element — stdin has no OS-level length limit, unlike a single argv
/// string (Linux's `MAX_ARG_STRLEN`, 128 KiB on common configurations; see
/// item #75). Writing on a dedicated thread, symmetric with the stdout/stderr
/// readers above, avoids a deadlock if the child starts producing output
/// before `stdin` is fully written and the OS pipe buffer fills up.
#[allow(dead_code)]
pub fn run_captured(
    mut cmd: Command,
    hard_cap: Duration,
    idle_timeout: Duration,
    stdin: Option<&str>,
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
    let mut child = cmd.spawn()?;

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

    let mut pipe = child.stdout.take().expect("stdout piped above");
    let stdout_activity = activity.clone();
    let reader = std::thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        // Read in chunks (rather than `read_to_string` straight to EOF) so
        // `activity` reflects bytes as they arrive, not only once the child
        // exits — the wait loop below needs that to detect a stalled child
        // before it closes its pipes.
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    stdout_activity.fetch_add(n as u64, Ordering::Relaxed);
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    });
    let mut err_pipe = child.stderr.take().expect("stderr piped above");
    let stderr_activity = activity.clone();
    let err_reader = std::thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match err_pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    stderr_activity.fetch_add(n as u64, Ordering::Relaxed);
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
        if last_activity_at.elapsed() >= idle_timeout {
            kill_tree(&mut child);
            let status = child.wait()?;
            timed_out = true;
            idle_killed = true;
            break status;
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    let stdout = reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    Ok(Captured {
        success: status.success() && !timed_out,
        stdout,
        stderr,
        timed_out,
        idle_killed,
    })
}

/// Build the full argv for a headless run: `[binary, <print-mode flags…>,
/// <extra args…>]`. `None` if the agent has no headless print mode. The
/// prompt is deliberately not included here — it's piped over the child's
/// stdin by `run_headless` instead of appended as an argv element, since a
/// single argv string is capped at Linux's `MAX_ARG_STRLEN` (128 KiB on
/// common configurations) while stdin has no such limit (item #75). All four
/// headless-mapped agents (`claude -p`, `gemini -p`, `codex exec`, `opencode
/// run`) read the prompt from stdin when none is given positionally.
#[allow(dead_code)]
pub fn headless_argv(agent: Agent, binary: &Path, extra_args: &[String]) -> Option<Vec<String>> {
    let flags = headless_args(agent)?;
    let mut argv = Vec::with_capacity(flags.len() + extra_args.len() + 1);
    argv.push(binary.to_string_lossy().into_owned());
    argv.extend(flags.iter().map(|s| (*s).to_string()));
    argv.extend(extra_args.iter().cloned());
    Some(argv)
}

/// For a script-shim binary (`.ps1`, or a `.cmd`/`.bat` that wraps a sibling
/// `.ps1` — cursor-agent's shim shape), returns the `powershell.exe` launcher
/// that runs it with `-WindowStyle Hidden`. Script shims chain through
/// `powershell.exe` (and often a bundled `node.exe`) and need a real console
/// to run — under `CREATE_NO_WINDOW` cursor-agent hangs with zero output and
/// never produces a reply — but `-WindowStyle Hidden` provides that console
/// with the window hidden, so no terminal flashes over the user's desktop.
/// Returns `None` for native binaries (handled by the normal
/// `CREATE_NO_WINDOW` path) and for `.cmd`/`.bat` with no `.ps1` sibling.
#[cfg(windows)]
fn script_shim_launcher(binary: &Path, args: &[String]) -> Option<(String, Vec<String>)> {
    let script = match binary.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "ps1" => binary.to_path_buf(),
        "cmd" | "bat" => {
            let ps1 = binary.with_extension("ps1");
            if ps1.is_file() {
                ps1
            } else {
                return None;
            }
        }
        _ => return None,
    };
    let mut argv = vec![
        "powershell.exe".to_string(),
        "-NoProfile".to_string(),
        "-ExecutionPolicy".to_string(),
        "Bypass".to_string(),
        "-WindowStyle".to_string(),
        "Hidden".to_string(),
        "-File".to_string(),
        script.to_string_lossy().into_owned(),
    ];
    argv.extend(args.iter().cloned());
    Some(("powershell.exe".to_string(), argv))
}

/// Resolves `(command, args, hidden_console)` for a headless spawn: the script
/// shim launcher on Windows when one applies (`hidden_console = true`, meaning
/// window hiding is already handled), else the binary itself. `args` is the
/// print-mode flags + extra args (no prompt — that's piped via stdin).
fn launch_command(binary: &Path, args: &[String]) -> (String, Vec<String>, bool) {
    #[cfg(windows)]
    if let Some((cmd, argv)) = script_shim_launcher(binary, args) {
        return (cmd, argv, true);
    }
    (binary.to_string_lossy().into_owned(), args.to_vec(), false)
}

/// Outcome of a headless (non-interactive, output-captured) agent invocation.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HeadlessReply {
    pub text: String,
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum HeadlessOutcome {
    Ok(HeadlessReply),
    UnknownAgent(String),
    /// The agent has no non-interactive print mode.
    NotHeadless(String),
    /// The agent binary was not found on PATH.
    NotFound(String),
    /// The agent ran but failed (non-zero exit or timed out).
    Failed(String),
}

fn parse_json_reply(stdout: &str) -> HeadlessReply {
    match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        Ok(value) => HeadlessReply {
            text: value
                .get("result")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(stdout)
                .to_string(),
            session_id: value
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            cost_usd: value
                .get("total_cost_usd")
                .and_then(serde_json::Value::as_f64),
        },
        Err(_) => HeadlessReply {
            text: stdout.to_string(),
            session_id: None,
            cost_usd: None,
        },
    }
}

/// Run an agent non-interactively with `prompt` and capture its reply, killing
/// it if it outlives `hard_cap` or goes `idle_timeout` without new output
/// (see `run_captured`'s doc comment for the distinction). Reuses the shared
/// registry (binary discovery + per-agent print-mode mapping) so callers
/// don't reimplement any of it.
#[allow(dead_code)]
pub fn run_headless(
    registry: &[AgentSpec],
    agent: &str,
    prompt: &str,
    hard_cap: Duration,
    idle_timeout: Duration,
    extra_args: &[String],
    request_json: bool,
) -> HeadlessOutcome {
    let Some(spec) = registry.iter().find(|s| s.id.as_str() == agent) else {
        return HeadlessOutcome::UnknownAgent(format!("unknown agent: {agent}"));
    };
    // Check headless support before touching PATH, so an unmapped agent reports
    // NotHeadless rather than NotFound.
    if headless_args(spec.id).is_none() {
        return HeadlessOutcome::NotHeadless(format!(
            "{} has no headless print mode",
            spec.display_name
        ));
    }
    let Some(binary) = find_binary(spec.binary_names) else {
        return HeadlessOutcome::NotFound(format!(
            "{} not found on PATH — install it first with: agentflare agents install {agent}",
            spec.binary_names.join(" / ")
        ));
    };
    let mut full_args: Vec<String> = Vec::with_capacity(extra_args.len() + 2);
    if request_json && let Some(flags) = json_output_args(spec.id) {
        full_args.extend(flags.iter().map(|s| (*s).to_string()));
    }
    full_args.extend(extra_args.iter().cloned());
    let Some(argv) = headless_argv(spec.id, &binary, &full_args) else {
        return HeadlessOutcome::NotHeadless(format!(
            "{} has no headless print mode",
            spec.display_name
        ));
    };
    // Native Linux and WSL2 run the agent CLI inside a bwrap sandbox, same as
    // `Supervisor::spawn`; Windows and macOS get argv back unchanged (see
    // `agentflare_jobs::sandbox`). Uses the ambient cwd since neither this
    // function nor `run_captured` below ever calls `Command::current_dir` —
    // the child inherits whatever directory the caller already chdir'd into
    // (e.g. `execute_work`'s worktree chdir for autonomous dispatch).
    //
    // `git_writable = true`: unlike `Supervisor::spawn`'s arbitrary job
    // commands, this specific child IS the headless coding-agent CLI, whose
    // entire job is `git add`/`git commit`/`git push` its own work. Item
    // #88: re-protecting `.git` read-only here (the sandbox's default) made
    // every headless work-item dispatch fail that step with "Read-only file
    // system" -- which `flare_git_core::worktree::commit_uncommitted` then
    // swallowed silently, so `agentflare_mcp::item_done` fell through to
    // "nothing was ever committed" and reported success anyway (exit 0),
    // leaving real staged/edited work stranded in the worktree.
    let cwd = std::env::current_dir().ok();
    // Script-shim agents (`.ps1`, or `.cmd`/`.bat` wrapping a sibling
    // `.ps1`) are launched through `powershell.exe -WindowStyle Hidden`
    // rather than the shim itself — see `launch_command`. The rest (native
    // `.exe`) run directly under `CREATE_NO_WINDOW` below.
    #[cfg(windows)]
    let (launch_cmd, launch_args, hidden_console) = launch_command(&binary, &argv[1..]);
    #[cfg(not(windows))]
    let (launch_cmd, launch_args, _hidden_console) = launch_command(&binary, &argv[1..]);
    let (sandboxed_command, sandboxed_args) =
        agentflare_jobs::sandbox::wrap(&launch_cmd, &launch_args, cwd.as_deref(), true);
    let mut cmd = Command::new(&sandboxed_command);
    cmd.args(&sandboxed_args);
    // Suppress the console window the daemon's child would otherwise flash —
    // but only for native binaries. Script shims need a real console to run
    // (under `CREATE_NO_WINDOW` cursor-agent hangs with zero output); the
    // powershell launcher's `-WindowStyle Hidden` hides *its* window instead,
    // so `hidden_console` short-circuits this. `run_captured` no longer
    // applies this itself; it's per-agent here.
    #[cfg(windows)]
    if !hidden_console {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    // See the matching strip in `run_launch_env` above (item #139) — same
    // rationale applies to headless child processes.
    cmd.env_remove("CARGO_TARGET_DIR");
    // Explicit, not inherited from this process's own ambient env: when the
    // agent shells out to `git`, the `flare-git-shim` on its PATH classifies
    // bypass eligibility by `AGENTFLARE_AGENT` (see `flare-git-core::classify`).
    // A subprocess-per-`agentflare work`-invocation caller already has this
    // set correctly in its own ambient env and this is a no-op for it, but a
    // caller running multiple work items as threads inside one long-lived
    // process (item #19's in-process dispatch) has no single ambient value
    // that's correct for all of them — only an explicit per-spawn env var is.
    cmd.env("AGENTFLARE_AGENT", spec.id.as_str());
    match run_captured(cmd, hard_cap, idle_timeout, Some(prompt)) {
        Ok(c) if c.success => {
            if request_json && json_output_args(spec.id).is_some() {
                HeadlessOutcome::Ok(parse_json_reply(&c.stdout))
            } else {
                HeadlessOutcome::Ok(HeadlessReply {
                    text: c.stdout,
                    session_id: None,
                    cost_usd: None,
                })
            }
        }
        Ok(c) if c.timed_out => {
            let reason = if c.idle_killed {
                format!("went idle for {idle_timeout:?} (no new output)")
            } else {
                format!("exceeded hard cap of {hard_cap:?}")
            };
            HeadlessOutcome::Failed(format!(
                "{} timed out — {reason}{}",
                spec.display_name,
                diagnostic_suffix(&c)
            ))
        }
        Ok(c) => HeadlessOutcome::Failed(format!(
            "{} exited non-zero{}",
            spec.display_name,
            diagnostic_suffix(&c)
        )),
        Err(e) => HeadlessOutcome::Failed(format!("failed to run {}: {e}", spec.display_name)),
    }
}

/// Claude Code's `--output-format stream-json` reply shape: one JSON object
/// per line (system init, tool_use/tool_result, assistant messages, ...),
/// with only the FINAL line carrying `{"result": "...", "session_id": "...",
/// "total_cost_usd": 0.0}` — the same shape the single-object `json` format
/// uses for its one and only line, so parsing "the last line" handles both.
/// Falls back to the raw text unparsed for any agent/output whose last line
/// isn't that exact JSON shape — never errors, never blocks the caller.
///
/// Restores what `9dd859e`/`7698fbc` wired into the old `coder` step and
/// `5c46bd3` deleted as apparently-dead code when that step was replaced by
/// `sdd_loop` (see item #489) — `work_item_pipeline::real_agent_send_hook`
/// never regained an equivalent call, so every Claude Code reply (implementer,
/// reviewer, AND the judge) became the raw multi-line stream-json transcript.
/// For the judge specifically, `parse_judge_decision` then parses the
/// transcript's first line — the `{"type":"system","subtype":"init",...}`
/// event, which has no `action` field — instead of the judge's actual
/// decision on the transcript's last line, hard-failing every judge turn.
pub(crate) fn parse_claude_reply(raw: &str) -> (String, Option<String>, Option<f64>) {
    let last_line = raw.trim().lines().next_back().unwrap_or("");
    match serde_json::from_str::<serde_json::Value>(last_line) {
        Ok(v) => {
            let text = v
                .get("result")
                .and_then(|r| r.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| raw.to_string());
            let session_id = v
                .get("session_id")
                .and_then(|s| s.as_str())
                .map(str::to_string);
            let cost = v.get("total_cost_usd").and_then(serde_json::Value::as_f64);
            (text, session_id, cost)
        }
        Err(_) => (raw.to_string(), None, None),
    }
}

/// Every role dispatched through `real_agent_send_hook` (`work_item_pipeline`)
/// — implementer, task-reviewer, re-reviewer, AND the judge — shares that
/// hook, and `build_extra_args` (`cli::work`) forces `--output-format
/// stream-json` for Claude Code regardless of role. `run_headless`'s captured
/// stdout is therefore that raw multi-line transcript, not plain reply text;
/// `parse_claude_reply` is what pulls the real reply off its final line.
/// Other agents' headless output is already plain text, so this is a no-op
/// for them. See `parse_claude_reply`'s doc comment for why this call exists
/// at all (item #489).
pub(crate) fn clean_agent_reply(agent: &str, raw: String) -> String {
    if agent == Agent::ClaudeCode.as_str() {
        parse_claude_reply(&raw).0
    } else {
        raw
    }
}

/// The captured child's own output is real, useful diagnostic evidence of
/// what it was doing right up to the kill — dropping it (the old behavior)
/// turned every timeout into a black box with no way to tell "made real
/// progress and got killed mid-verification" apart from "never did anything."
/// Prefers stdout (the agent's actual reply stream); falls back to stderr
/// when stdout is empty.
fn diagnostic_suffix(captured: &Captured) -> String {
    let (label, text) = if !captured.stdout.is_empty() {
        ("stdout", captured.stdout.as_str())
    } else if !captured.stderr.is_empty() {
        ("stderr", captured.stderr.as_str())
    } else {
        return " (no output captured)".to_string();
    };
    format!(
        " — last {label} before kill:\n{}",
        tail_str(text, DIAGNOSTIC_TAIL_CHARS)
    )
}

/// Also reused by `cli::work` to cap a headless run's reply before it's
/// embedded in a success comment (item #81) — same "bounded tail, not the
/// whole capture" spirit as this module's own timeout/failure diagnostics.
pub(crate) const DIAGNOSTIC_TAIL_CHARS: usize = 2000;

/// The last `max_chars` characters of `s`, UTF-8-boundary-safe (never slices
/// through the middle of a multi-byte character).
pub(crate) fn tail_str(s: &str, max_chars: usize) -> &str {
    match s.char_indices().rev().nth(max_chars.saturating_sub(1)) {
        Some((idx, _)) => &s[idx..],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use agent_registry::detect::PATH_LOCK as GLOBAL_STATE_LOCK;
    use agent_registry::{Agent, Tier};

    fn test_registry() -> Vec<AgentSpec> {
        vec![
            AgentSpec {
                id: Agent::Aider,
                display_name: "aider",
                tier: Tier::Cli,
                binary_names: &["aider"],
                version_args: &[],
                package_manager: None,
                package_name: None,
            },
            AgentSpec {
                id: Agent::Cline,
                display_name: "cline",
                tier: Tier::Extension,
                binary_names: &[],
                version_args: &[],
                package_manager: None,
                package_name: None,
            },
        ]
    }

    #[test]
    fn launch_unknown_agent_errors() {
        let reg = test_registry();
        match run_launch(&reg, "nonexistent", None, None, &[]) {
            LaunchOutcome::UnknownAgent(msg) => assert!(msg.contains("nonexistent")),
            _ => panic!("expected UnknownAgent"),
        }
    }

    #[test]
    fn launch_extension_agent_errors() {
        let reg = test_registry();
        match run_launch(&reg, "cline", None, None, &[]) {
            LaunchOutcome::Extension(msg) => assert!(msg.contains("editor extension")),
            _ => panic!("expected Extension"),
        }
    }

    #[test]
    fn launch_not_on_path_errors() {
        let reg = test_registry();
        // "aider" unlikely to be on a test PATH
        match run_launch(&reg, "aider", None, None, &[]) {
            LaunchOutcome::NotFound(msg) => assert!(msg.contains("not found on PATH")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    // Process spawning is exercised via a POSIX shell; gate to Unix so the
    // Windows CI (no `sh`/`sleep`) never runs it.
    #[cfg(unix)]
    #[test]
    fn run_captured_captures_stdout() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'hello world'");
        let out = run_captured(
            cmd,
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(5),
            None,
        )
        .unwrap();
        assert!(out.success);
        assert!(!out.timed_out);
        assert_eq!(out.stdout, "hello world");
    }

    #[cfg(unix)]
    #[test]
    fn run_captured_keeps_output_written_before_a_timeout_kill() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'made it here'; sleep 5");
        let out = run_captured(
            cmd,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_millis(150),
            None,
        )
        .unwrap();
        assert!(out.timed_out);
        assert_eq!(
            out.stdout, "made it here",
            "output written before the kill must still be captured, not discarded"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_captured_times_out_and_kills_the_child() {
        let start = std::time::Instant::now();
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 5");
        let out = run_captured(
            cmd,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_millis(150),
            None,
        )
        .unwrap();
        assert!(out.timed_out, "should report timeout");
        assert!(!out.success, "a killed child is not a success");
        // Must return promptly, not wait out the full 5s sleep.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "did not kill promptly"
        );
    }

    // A process that keeps producing output must NOT be killed just because
    // it has run past what used to be the single fixed timeout — only a
    // genuinely stalled child (no new bytes for `idle_timeout`) should be.
    // This is the behavior item #20 exists to add.
    #[cfg(unix)]
    #[test]
    fn run_captured_does_not_idle_timeout_while_output_keeps_arriving() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("for i in 1 2 3 4 5; do printf 'x'; sleep 0.05; done");
        let out = run_captured(
            cmd,
            std::time::Duration::from_secs(30),
            std::time::Duration::from_millis(300),
            None,
        )
        .unwrap();
        assert!(
            !out.timed_out,
            "steady output growth must reset the idle clock, not get killed"
        );
        assert_eq!(out.stdout, "xxxxx");
    }

    // Mirrors `run_captured_keeps_output_written_before_a_timeout_kill` but
    // with a generous hard cap and a tight idle window, proving the idle
    // timeout — not the hard cap — is what catches a child that produced
    // real output and then went quiet.
    #[cfg(unix)]
    #[test]
    fn run_captured_idle_timeout_kills_a_stalled_child_well_before_the_hard_cap() {
        let start = std::time::Instant::now();
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'did some work'; sleep 5");
        let out = run_captured(
            cmd,
            std::time::Duration::from_secs(30),
            std::time::Duration::from_millis(150),
            None,
        )
        .unwrap();
        assert!(out.timed_out, "should time out");
        assert!(
            out.idle_killed,
            "should be killed for going idle, not for exceeding the hard cap"
        );
        assert_eq!(out.stdout, "did some work");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "idle timeout should fire well before the 30s hard cap"
        );
    }

    // Proves the fix for the "descendant outlives the direct child" hang: the
    // direct child backgrounds a grandchild that inherits the piped stdout fd,
    // then waits on it. If timeout only killed the direct child (the old
    // `child.kill()` behavior), the grandchild would keep the pipe's write end
    // open and `reader.join()` would block for the full 5s sleep. With
    // process-group tree-killing, both die together and this returns promptly.
    #[cfg(unix)]
    #[test]
    fn run_captured_times_out_and_kills_the_whole_tree() {
        let start = std::time::Instant::now();
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 5 & wait");
        let out = run_captured(
            cmd,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_millis(150),
            None,
        )
        .unwrap();
        assert!(out.timed_out, "should report timeout");
        assert!(!out.success, "a killed child is not a success");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "did not kill the whole tree promptly — a descendant likely kept the stdout pipe open"
        );
    }

    // Item #139: an ambient CARGO_TARGET_DIR must never reach the launched
    // agent — Cargo's env var always outranks the worktree's isolated
    // `.cargo/config.toml` (see `isolate_worktree_target_dir` in
    // worktree.rs), so leaking it here would silently defeat that isolation
    // for every build the agent runs. `env_remove` clones the current env
    // and drops the key at that point, so this holds regardless of what any
    // other test concurrently does to the ambient var.
    #[cfg(unix)]
    #[test]
    fn run_launch_env_strips_ambient_cargo_target_dir() {
        // SAFETY: GLOBAL_STATE_LOCK serializes this process-wide env
        // mutation against every other test touching CARGO_TARGET_DIR/PATH.
        let _guard = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let marker = tempfile::NamedTempFile::new().unwrap();
        let marker_path = marker.path().to_path_buf();
        unsafe {
            std::env::set_var("CARGO_TARGET_DIR", "/tmp/shared-target");
        }
        let reg = vec![AgentSpec {
            id: Agent::Aider,
            display_name: "aider",
            tier: Tier::Cli,
            binary_names: &["sh"],
            version_args: &[],
            package_manager: None,
            package_name: None,
        }];
        // `printf`, not `echo -n`: on macOS's /bin/sh, `-n` isn't a flag
        // (that's a bash builtin behavior), so `echo -n` would literally
        // print "-n" into the marker instead of suppressing the newline.
        let script = format!(
            "printf '%s' \"$CARGO_TARGET_DIR\" > {}",
            marker_path.display()
        );
        run_launch_env(
            &reg,
            "aider",
            None,
            None,
            &["-c".to_string(), script],
            &[],
            false,
        );
        unsafe {
            std::env::remove_var("CARGO_TARGET_DIR");
        }
        let content = std::fs::read_to_string(&marker_path).unwrap();
        assert_eq!(
            content, "",
            "child must not inherit ambient CARGO_TARGET_DIR"
        );
    }

    // An explicit CARGO_TARGET_DIR passed in `env` (e.g. sourced from a
    // project's `.dev.vars`) must not reintroduce the var `env_remove`
    // above just stripped, or a dev-vars file that happens to set it would
    // silently defeat the ambient-isolation guarantee this launch path
    // exists for.
    #[cfg(unix)]
    #[test]
    fn run_launch_env_override_cannot_reintroduce_cargo_target_dir() {
        // SAFETY: GLOBAL_STATE_LOCK serializes against other tests that
        // mutate CARGO_TARGET_DIR in the ambient process env.
        let _guard = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let marker = tempfile::NamedTempFile::new().unwrap();
        let marker_path = marker.path().to_path_buf();
        let reg = vec![AgentSpec {
            id: Agent::Aider,
            display_name: "aider",
            tier: Tier::Cli,
            binary_names: &["sh"],
            version_args: &[],
            package_manager: None,
            package_name: None,
        }];
        let script = format!(
            "printf '%s' \"$CARGO_TARGET_DIR\" > {}",
            marker_path.display()
        );
        run_launch_env(
            &reg,
            "aider",
            None,
            None,
            &["-c".to_string(), script],
            &[(
                "CARGO_TARGET_DIR".to_string(),
                "/should/not/leak".to_string(),
            )],
            false,
        );
        let content = std::fs::read_to_string(&marker_path).unwrap();
        assert_eq!(
            content, "",
            "explicit env override must not reintroduce CARGO_TARGET_DIR"
        );
    }

    fn headless_registry() -> Vec<AgentSpec> {
        vec![
            AgentSpec {
                id: Agent::Codex,
                display_name: "codex",
                tier: Tier::Cli,
                // A binary name that will never resolve on PATH.
                binary_names: &["definitely-not-a-real-binary-xyz"],
                version_args: &[],
                package_manager: None,
                package_name: None,
            },
            AgentSpec {
                id: Agent::Aider, // Cli, but no headless print mode mapped
                display_name: "aider",
                tier: Tier::Cli,
                binary_names: &["aider"],
                version_args: &[],
                package_manager: None,
                package_name: None,
            },
        ]
    }

    #[test]
    fn headless_argv_carries_no_prompt_element() {
        // The prompt is piped over stdin by `run_headless`, not appended to
        // argv (item #75) — argv is just the binary plus print-mode flags.
        let binary = std::path::Path::new("/usr/bin/claude");
        assert_eq!(
            headless_argv(Agent::ClaudeCode, binary, &[]),
            Some(vec!["/usr/bin/claude".to_string(), "-p".to_string()])
        );
        assert_eq!(
            headless_argv(Agent::Codex, std::path::Path::new("/x/codex"), &[]),
            Some(vec!["/x/codex".to_string(), "exec".to_string()])
        );
    }

    #[test]
    fn headless_argv_none_without_print_mode() {
        assert_eq!(
            headless_argv(Agent::Aider, std::path::Path::new("/x/aider"), &[]),
            None
        );
    }

    #[test]
    fn run_headless_unknown_agent() {
        let reg = headless_registry();
        match run_headless(
            &reg,
            "nope",
            "hi",
            Duration::from_secs(1),
            Duration::from_secs(1),
            &[],
            false,
        ) {
            HeadlessOutcome::UnknownAgent(m) => assert!(m.contains("nope")),
            other => panic!("expected UnknownAgent, got {other:?}"),
        }
    }

    #[test]
    fn run_headless_agent_without_print_mode() {
        let reg = headless_registry();
        match run_headless(
            &reg,
            "aider",
            "hi",
            Duration::from_secs(1),
            Duration::from_secs(1),
            &[],
            false,
        ) {
            HeadlessOutcome::NotHeadless(m) => assert!(m.contains("aider")),
            other => panic!("expected NotHeadless, got {other:?}"),
        }
    }

    #[test]
    #[ignore]
    fn repro_cursor_cmd_hang() {
        let t0 = std::time::Instant::now();
        let out = super::run_headless(
            agent_registry::REGISTRY,
            "cursor",
            "Reply with exactly: ok",
            Duration::from_secs(120),
            Duration::from_secs(60),
            &["--force".to_string()],
            false,
        );
        eprintln!("elapsed={:?}", t0.elapsed());
        match out {
            super::HeadlessOutcome::Ok(reply) => eprintln!("OK reply={:?}", reply),
            other => eprintln!("NOT OK: {other:?}"),
        }
    }

    #[test]
    fn run_headless_binary_not_found() {
        let reg = headless_registry();
        match run_headless(
            &reg,
            "codex",
            "hi",
            Duration::from_secs(1),
            Duration::from_secs(1),
            &[],
            false,
        ) {
            HeadlessOutcome::NotFound(m) => assert!(m.contains("not found")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    // Item #75: a prompt built from an item's full comment history can
    // exceed Linux's MAX_ARG_STRLEN (128 KiB on common configurations), the
    // max length of any single argv element — separate from and much
    // smaller than total ARG_MAX. Passing the prompt as a trailing argv
    // element (the old behavior) made `Command::spawn()` fail with E2BIG
    // once a prompt crossed that ceiling. `sh -p -c 'wc -c'` stands in for a
    // headless agent binary: `-p` is `headless_args(ClaudeCode)`'s flag
    // (harmless to `sh`, which treats it as the POSIX "privileged" option),
    // and `wc -c` reports exactly how many bytes it received on stdin —
    // proving the whole oversized prompt arrived intact via stdin rather
    // than argv.
    #[cfg(unix)]
    #[test]
    fn run_headless_pipes_a_prompt_over_the_argv_length_limit_via_stdin() {
        let reg = vec![AgentSpec {
            id: Agent::ClaudeCode,
            display_name: "claude-code",
            tier: Tier::Cli,
            binary_names: &["sh"],
            version_args: &[],
            package_manager: None,
            package_name: None,
        }];
        // Linux's MAX_ARG_STRLEN is 128 KiB (32 * 4 KiB pages); comfortably
        // exceed it so this would have hit E2BIG under the old argv-based
        // prompt delivery.
        let huge_prompt = "x".repeat(200 * 1024);
        match run_headless(
            &reg,
            "claude-code",
            &huge_prompt,
            Duration::from_secs(10),
            Duration::from_secs(10),
            &["-c".to_string(), "wc -c".to_string()],
            false,
        ) {
            HeadlessOutcome::Ok(reply) => {
                assert_eq!(
                    reply.text.trim(),
                    huge_prompt.len().to_string(),
                    "the full oversized prompt should have arrived via stdin"
                );
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn parse_claude_reply_extracts_structured_fields() {
        let raw = r#"{"result":"Fixed the race by adding a mutex.","session_id":"sess-123","total_cost_usd":0.0842}"#;
        let (text, session_id, cost) = parse_claude_reply(raw);
        assert_eq!(text, "Fixed the race by adding a mutex.");
        assert_eq!(session_id.as_deref(), Some("sess-123"));
        assert_eq!(cost, Some(0.0842));
    }

    #[test]
    fn parse_claude_reply_extracts_the_result_from_the_last_line_of_a_stream_json_transcript() {
        // --output-format stream-json emits one JSON object per line (system
        // init, tool_use/tool_result, assistant messages, ...) and only the
        // FINAL line carries the same {"result":...} shape the single-object
        // `json` format uses — everything before it must be ignored, not
        // treated as (or blended into) the reply text. This is the exact
        // shape a judge's raw reply takes (item #489): the first line is a
        // valid-but-`action`-less JSON object, which `parse_judge_decision`
        // used to grab if this function wasn't called first.
        let raw = concat!(
            r#"{"type":"system","subtype":"init","session_id":"sess-123"}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"working..."}]}}"#,
            "\n",
            r#"{"type":"result","result":"Fixed the race by adding a mutex.","session_id":"sess-123","total_cost_usd":0.0842}"#,
        );
        let (text, session_id, cost) = parse_claude_reply(raw);
        assert_eq!(text, "Fixed the race by adding a mutex.");
        assert_eq!(session_id.as_deref(), Some("sess-123"));
        assert_eq!(cost, Some(0.0842));
    }

    #[test]
    fn clean_agent_reply_is_a_no_op_for_non_claude_agents() {
        let raw = "DONE: added the flag".to_string();
        assert_eq!(
            clean_agent_reply(Agent::Opencode.as_str(), raw.clone()),
            raw
        );
    }

    #[test]
    fn parse_claude_reply_falls_back_to_raw_text_on_non_json() {
        let raw = "plain text reply, no JSON here";
        let (text, session_id, cost) = parse_claude_reply(raw);
        assert_eq!(text, raw);
        assert!(session_id.is_none());
        assert!(cost.is_none());
    }

    #[test]
    fn tail_str_returns_the_whole_string_when_shorter_than_the_limit() {
        assert_eq!(tail_str("hello", 100), "hello");
    }

    #[test]
    fn tail_str_returns_only_the_last_n_chars() {
        assert_eq!(tail_str("abcdefgh", 3), "fgh");
    }

    #[test]
    fn tail_str_does_not_panic_on_a_multibyte_boundary() {
        // Each of these is a 3-4 byte UTF-8 char; slicing by raw byte offset
        // instead of char boundary would panic here.
        let s = "a€b€c€d€e€f€g€h€i€j";
        // Must not panic, and must return valid, non-empty UTF-8.
        let tail = tail_str(s, 5);
        assert!(!tail.is_empty());
        assert!(tail.chars().count() <= 5);
    }

    #[test]
    fn diagnostic_suffix_reports_no_output_captured_when_both_streams_are_empty() {
        let c = Captured {
            success: false,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
            idle_killed: false,
        };
        assert_eq!(diagnostic_suffix(&c), " (no output captured)");
    }

    #[test]
    fn diagnostic_suffix_prefers_stdout_over_stderr() {
        let c = Captured {
            success: false,
            stdout: "working on task 3...".to_string(),
            stderr: "some warning".to_string(),
            timed_out: true,
            idle_killed: false,
        };
        let suffix = diagnostic_suffix(&c);
        assert!(suffix.contains("last stdout before kill"));
        assert!(suffix.contains("working on task 3..."));
    }

    #[test]
    fn diagnostic_suffix_falls_back_to_stderr_when_stdout_is_empty() {
        let c = Captured {
            success: false,
            stdout: String::new(),
            stderr: "panic: something broke".to_string(),
            timed_out: true,
            idle_killed: false,
        };
        let suffix = diagnostic_suffix(&c);
        assert!(suffix.contains("last stderr before kill"));
        assert!(suffix.contains("panic: something broke"));
    }

    #[test]
    fn failed_non_zero_exit_includes_captured_output() {
        let captured = Captured {
            stdout: String::new(),
            stderr: "HTTP 429 Too Many Requests".to_string(),
            success: false,
            timed_out: false,
            idle_killed: false,
        };
        let msg = match Ok::<_, std::io::Error>(captured) {
            Ok(c) if c.success => unreachable!(),
            Ok(c) if c.timed_out => unreachable!(),
            Ok(c) => format!("test exited non-zero{}", diagnostic_suffix(&c)),
            Err(_) => unreachable!(),
        };
        assert!(
            msg.contains("HTTP 429 Too Many Requests"),
            "expected captured stderr in the failure message, got: {msg}"
        );
    }

    #[test]
    fn parse_json_reply_extracts_result_session_and_cost() {
        let reply = parse_json_reply(
            r#"{"type":"result","result":"pong","session_id":"abc-123","total_cost_usd":0.05}"#,
        );
        assert_eq!(reply.text, "pong");
        assert_eq!(reply.session_id.as_deref(), Some("abc-123"));
        assert_eq!(reply.cost_usd, Some(0.05));
    }

    #[test]
    fn parse_json_reply_handles_missing_cost_usd() {
        let reply = parse_json_reply(r#"{"type":"result","result":"pong","session_id":"abc-123"}"#);
        assert_eq!(reply.text, "pong");
        assert_eq!(reply.session_id.as_deref(), Some("abc-123"));
        assert_eq!(reply.cost_usd, None);
    }

    #[test]
    fn parse_json_reply_falls_back_to_raw_text_on_malformed_json() {
        let reply = parse_json_reply("not json at all");
        assert_eq!(reply.text, "not json at all");
        assert_eq!(reply.session_id, None);
        assert_eq!(reply.cost_usd, None);
    }

    #[cfg(unix)]
    #[test]
    fn run_headless_with_request_json_parses_a_json_reply_from_the_child() {
        use std::os::unix::fs::PermissionsExt;
        // This file's usual `sh -c <script>` stub-agent idiom doesn't work
        // with `request_json: true`: it prepends `--output-format json`
        // ahead of `extra_args`, and a real `sh`/bash rejects that as an
        // unrecognized long option before ever reaching `-c` -- so instead
        // this fake binary ignores its argv entirely and always emits a
        // fixed JSON reply. It lives under this crate's own `target/` dir,
        // not the system temp dir -- `run_headless`'s bwrap sandbox remounts
        // `/tmp` as a private, empty tmpfs invisible to the child (see
        // `flare_sandbox::bwrap`'s `--tmpfs /tmp`), while `target/` sits
        // under the sandboxed cwd bind and stays visible and writable.
        let dir = tempfile::Builder::new()
            .prefix("agentflare-fake-claude-")
            .tempdir_in(std::env::current_dir().unwrap().join("target"))
            .unwrap();
        let fake_bin = dir.path().join("fake-claude");
        std::fs::write(
            &fake_bin,
            "#!/bin/sh\necho '{\"type\":\"result\",\"result\":\"pong\",\"session_id\":\"sess-xyz\"}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let binary_names: &'static [&'static str] = Box::leak(
            vec![&*Box::leak(
                fake_bin.to_string_lossy().into_owned().into_boxed_str(),
            )]
            .into_boxed_slice(),
        );

        let reg = vec![AgentSpec {
            id: Agent::ClaudeCode,
            display_name: "claude-code",
            tier: Tier::Cli,
            binary_names,
            version_args: &[],
            package_manager: None,
            package_name: None,
        }];
        match run_headless(
            &reg,
            "claude-code",
            "hi",
            Duration::from_secs(10),
            Duration::from_secs(10),
            &[],
            true,
        ) {
            HeadlessOutcome::Ok(reply) => {
                assert_eq!(reply.text, "pong");
                assert_eq!(reply.session_id.as_deref(), Some("sess-xyz"));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }
}
