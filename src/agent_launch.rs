// Launch engine for `agentflare agents launch <agent> [args...]`.
// Finds the agent binary on PATH, maps --model/--mode to agent-native
// flags, and executes with pass-through args and inherited stdio.
use agent_registry::detect::find_binary;
use agent_registry::{Agent, AgentSpec, Tier, headless_args};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
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
    // runs. An explicit override in `env` below can still re-set it.
    cmd.env_remove("CARGO_TARGET_DIR");
    for (k, v) in env {
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
    /// True iff the child was killed for outliving the timeout.
    pub timed_out: bool,
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
/// whole process tree) if it outlives `timeout` (reporting `timed_out`).
/// Stdout is drained on a separate thread so a child that fills the OS pipe
/// buffer can't deadlock the wait loop.
#[allow(dead_code)]
pub fn run_captured(mut cmd: Command, timeout: Duration) -> std::io::Result<Captured> {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());
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

    let mut pipe = child.stdout.take().expect("stdout piped above");
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = pipe.read_to_string(&mut buf);
        buf
    });

    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= timeout {
            kill_tree(&mut child);
            let status = child.wait()?;
            timed_out = true;
            break status;
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    let stdout = reader.join().unwrap_or_default();
    Ok(Captured {
        success: status.success() && !timed_out,
        stdout,
        timed_out,
    })
}

/// Build the full argv for a headless run: `[binary, <print-mode flags…>, prompt]`.
/// `None` if the agent has no headless print mode.
#[allow(dead_code)]
pub fn headless_argv(agent: Agent, binary: &Path, prompt: &str) -> Option<Vec<String>> {
    let flags = headless_args(agent)?;
    let mut argv = Vec::with_capacity(flags.len() + 2);
    argv.push(binary.to_string_lossy().into_owned());
    argv.extend(flags.iter().map(|s| (*s).to_string()));
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
/// it if it outlives `timeout`. Reuses the shared registry (binary discovery +
/// per-agent print-mode mapping) so callers don't reimplement any of it.
#[allow(dead_code)]
pub fn run_headless(
    registry: &[AgentSpec],
    agent: &str,
    prompt: &str,
    timeout: Duration,
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
    let Some(argv) = headless_argv(spec.id, &binary, prompt) else {
        return HeadlessOutcome::NotHeadless(format!(
            "{} has no headless print mode",
            spec.display_name
        ));
    };
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    // See the matching strip in `run_launch_env` above (item #139) — same
    // rationale applies to headless child processes.
    cmd.env_remove("CARGO_TARGET_DIR");
    match run_captured(cmd, timeout) {
        Ok(c) if c.success => HeadlessOutcome::Ok(c.stdout),
        Ok(c) if c.timed_out => {
            HeadlessOutcome::Failed(format!("{} timed out after {timeout:?}", spec.display_name))
        }
        Ok(_) => HeadlessOutcome::Failed(format!("{} exited non-zero", spec.display_name)),
        Err(e) => HeadlessOutcome::Failed(format!("failed to run {}: {e}", spec.display_name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let out = run_captured(cmd, std::time::Duration::from_secs(5)).unwrap();
        assert!(out.success);
        assert!(!out.timed_out);
        assert_eq!(out.stdout, "hello world");
    }

    #[cfg(unix)]
    #[test]
    fn run_captured_times_out_and_kills_the_child() {
        let start = std::time::Instant::now();
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 5");
        let out = run_captured(cmd, std::time::Duration::from_millis(150)).unwrap();
        assert!(out.timed_out, "should report timeout");
        assert!(!out.success, "a killed child is not a success");
        // Must return promptly, not wait out the full 5s sleep.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "did not kill promptly"
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
        let out = run_captured(cmd, std::time::Duration::from_millis(150)).unwrap();
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
        let script = format!("echo -n \"$CARGO_TARGET_DIR\" > {}", marker_path.display());
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
            headless_argv(Agent::ClaudeCode, binary, "hi there"),
            Some(vec![
                "/usr/bin/claude".to_string(),
                "-p".to_string(),
                "hi there".to_string()
            ])
        );
        assert_eq!(
            headless_argv(Agent::Codex, std::path::Path::new("/x/codex"), "do it"),
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
            headless_argv(Agent::Aider, std::path::Path::new("/x/aider"), "p"),
            None
        );
    }

    #[test]
    fn run_headless_unknown_agent() {
        let reg = headless_registry();
        match run_headless(&reg, "nope", "hi", Duration::from_secs(1)) {
            HeadlessOutcome::UnknownAgent(m) => assert!(m.contains("nope")),
            other => panic!("expected UnknownAgent, got {other:?}"),
        }
    }

    #[test]
    fn run_headless_agent_without_print_mode() {
        let reg = headless_registry();
        match run_headless(&reg, "aider", "hi", Duration::from_secs(1)) {
            HeadlessOutcome::NotHeadless(m) => assert!(m.contains("aider")),
            other => panic!("expected NotHeadless, got {other:?}"),
        }
    }

    #[test]
    fn run_headless_binary_not_found() {
        let reg = headless_registry();
        match run_headless(&reg, "codex", "hi", Duration::from_secs(1)) {
            HeadlessOutcome::NotFound(m) => assert!(m.contains("not found")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }
}
