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
        let _ = flare_process::command("kill")
            .arg("-s")
            .arg("KILL")
            .arg("--")
            .arg(format!("-{}", child.id()))
            .status();
    }
    #[cfg(windows)]
    {
        let _ = flare_process::command("taskkill")
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
        let _ = flare_process::command("taskkill")
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
///
/// Runs in the caller's ambient cwd — the child inherits whatever directory
/// the caller already chdir'd into. Use `run_headless_in` when the cwd must
/// be set explicitly regardless of ambient state.
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
    run_headless_impl(
        None,
        registry,
        agent,
        prompt,
        hard_cap,
        idle_timeout,
        extra_args,
        request_json,
    )
}

/// Like `run_headless`, but explicitly sets the child's working directory to
/// `cwd` via `Command::current_dir` instead of relying on the caller's
/// ambient cwd. Needed because `agentflare_jobs::sandbox::wrap`'s own `cwd`
/// parameter only affects the Linux bwrap sandbox path (see its doc comment)
/// — on Windows/macOS the argv comes back unchanged and nothing else in this
/// module ever calls `Command::current_dir`.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub fn run_headless_in(
    cwd: &Path,
    registry: &[AgentSpec],
    agent: &str,
    prompt: &str,
    hard_cap: Duration,
    idle_timeout: Duration,
    extra_args: &[String],
    request_json: bool,
) -> HeadlessOutcome {
    run_headless_impl(
        Some(cwd),
        registry,
        agent,
        prompt,
        hard_cap,
        idle_timeout,
        extra_args,
        request_json,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_headless_impl(
    explicit_cwd: Option<&Path>,
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
    // `agentflare_jobs::sandbox`). `run_headless` (explicit_cwd = None) uses
    // the ambient cwd since neither this function nor `run_captured` below
    // otherwise calls `Command::current_dir` — the child inherits whatever
    // directory the caller already chdir'd into (e.g. `execute_work`'s
    // worktree chdir for autonomous dispatch). `run_headless_in` passes an
    // explicit cwd instead, set on `cmd` below.
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
    let cwd = explicit_cwd
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok());
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
    if let Some(dir) = explicit_cwd {
        cmd.current_dir(dir);
    }
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
/// stream-json` for Claude Code and cursor-agent regardless of role.
/// `run_headless`'s captured stdout is therefore that raw multi-line
/// transcript, not plain reply text; `parse_claude_reply` is what pulls the
/// real reply off its final line. Other agents' headless output is already
/// plain text, so this is a no-op for them. See `parse_claude_reply`'s doc
/// comment for why this call exists at all (item #489).
pub(crate) fn clean_agent_reply(agent: &str, raw: String) -> String {
    if agent == Agent::ClaudeCode.as_str() || agent == Agent::Cursor.as_str() {
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
    include!("agent_launch_tests.rs");
}
