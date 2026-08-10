// Launch engine for `agentflare agents launch <agent> [args...]`.
// Finds the agent binary on PATH, maps --model/--mode to agent-native
// flags, and executes with pass-through args and inherited stdio.
use agent_registry::detect::find_binary;
use agent_registry::{Agent, AgentSpec, Tier, headless_args};
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
#[allow(dead_code)]
pub fn run_captured(
    mut cmd: Command,
    hard_cap: Duration,
    idle_timeout: Duration,
) -> std::io::Result<Captured> {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.stdin(Stdio::null());
    // Make the child the leader of a new process group so any descendants it
    // spawns (which inherit the group by default) can be killed together via
    // `kill_tree` — see its doc comment for why this matters.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn()?;

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

/// Build the full argv for a headless run: `[binary, <print-mode flags…>, prompt]`.
/// `None` if the agent has no headless print mode.
#[allow(dead_code)]
pub fn headless_argv(
    agent: Agent,
    binary: &Path,
    prompt: &str,
    extra_args: &[String],
) -> Option<Vec<String>> {
    let flags = headless_args(agent)?;
    let mut argv = Vec::with_capacity(flags.len() + extra_args.len() + 2);
    argv.push(binary.to_string_lossy().into_owned());
    argv.extend(flags.iter().map(|s| (*s).to_string()));
    argv.extend(extra_args.iter().cloned());
    argv.push(prompt.to_string());
    Some(argv)
}

/// Outcome of a headless (non-interactive, output-captured) agent invocation.
#[allow(dead_code)]
#[derive(Debug)]
pub enum HeadlessOutcome {
    /// The agent ran and exited 0; carries captured stdout (the reply).
    Ok(String),
    UnknownAgent(String),
    /// The agent has no non-interactive print mode.
    NotHeadless(String),
    /// The agent binary was not found on PATH.
    NotFound(String),
    /// The agent ran but failed (non-zero exit or timed out).
    Failed(String),
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
    let Some(argv) = headless_argv(spec.id, &binary, prompt, extra_args) else {
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
    let cwd = std::env::current_dir().ok();
    let (sandboxed_command, sandboxed_args) =
        agentflare_jobs::sandbox::wrap(&argv[0], &argv[1..], cwd.as_deref());
    let mut cmd = Command::new(&sandboxed_command);
    cmd.args(&sandboxed_args);
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
    match run_captured(cmd, hard_cap, idle_timeout) {
        Ok(c) if c.success => HeadlessOutcome::Ok(c.stdout),
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

const DIAGNOSTIC_TAIL_CHARS: usize = 2000;

/// The last `max_chars` characters of `s`, UTF-8-boundary-safe (never slices
/// through the middle of a multi-byte character).
fn tail_str(s: &str, max_chars: usize) -> &str {
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
    fn headless_argv_puts_flags_before_prompt() {
        let binary = std::path::Path::new("/usr/bin/claude");
        assert_eq!(
            headless_argv(Agent::ClaudeCode, binary, "hi there", &[]),
            Some(vec![
                "/usr/bin/claude".to_string(),
                "-p".to_string(),
                "hi there".to_string()
            ])
        );
        assert_eq!(
            headless_argv(Agent::Codex, std::path::Path::new("/x/codex"), "do it", &[]),
            Some(vec![
                "/x/codex".to_string(),
                "exec".to_string(),
                "do it".to_string()
            ])
        );
    }

    #[test]
    fn headless_argv_none_without_print_mode() {
        assert_eq!(
            headless_argv(Agent::Aider, std::path::Path::new("/x/aider"), "p", &[]),
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
        ) {
            HeadlessOutcome::NotHeadless(m) => assert!(m.contains("aider")),
            other => panic!("expected NotHeadless, got {other:?}"),
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
        ) {
            HeadlessOutcome::NotFound(m) => assert!(m.contains("not found")),
            other => panic!("expected NotFound, got {other:?}"),
        }
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
}
