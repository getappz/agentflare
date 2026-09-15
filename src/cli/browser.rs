//! `agentflare browser` — agent-first browser automation (CLI side).
//!
//! Thin frontend over the `flare-browser` crate: every subcommand maps to
//! sidecar argv via [`flare_browser::build_argv`], so sessions, validation,
//! and output caps live in exactly one place (shared with the MCP tool).

use clap::{Args, Subcommand};

#[derive(Args)]
pub struct BrowserArgs {
    /// Browser session id. Defaults to `$AGENTFLARE_BROWSER_SESSION`, else a
    /// stable id derived from the cwd (`af-<hash>`) so concurrent worktrees
    /// each get an isolated browser session with zero config.
    #[arg(long)]
    pub session: Option<String>,
    /// Skip first-use auto-install (`mise install github:vercel-labs/agent-browser@latest`) and fail
    /// fast when the sidecar is missing. Env equivalent:
    /// `AGENTFLARE_BROWSER_NO_AUTO_INSTALL=1`.
    #[arg(long)]
    pub no_auto_install: bool,
    #[command(subcommand)]
    pub command: BrowserCommands,
}

#[derive(Subcommand)]
pub enum BrowserCommands {
    /// Launch the browser and navigate to a URL.
    Open { url: String },
    /// Accessibility tree with @e refs — the primary page read.
    Snapshot,
    /// Snapshot lines matching a query (deterministic observe).
    Observe {
        query: String,
        #[arg(long, default_value_t = 40)]
        limit: usize,
    },
    /// Click a ref or selector.
    Click { target: String },
    /// Clear and fill a field.
    Fill { target: String, text: String },
    /// Type into an element without clearing.
    Type { target: String, text: String },
    /// Press a key (Enter, Tab, Control+a).
    Press { key: String },
    /// Hover over an element.
    Hover { target: String },
    /// Select a dropdown option by value or label.
    Select { target: String, value: String },
    /// Check a checkbox.
    Check { target: String },
    /// Uncheck a checkbox.
    Uncheck { target: String },
    /// History back.
    Back,
    /// History forward.
    Forward,
    /// Reload the page.
    Reload,
    /// Read page state: get <text|html|value|title|url|...> [target].
    Get {
        kind: String,
        target: Option<String>,
    },
    /// Agent-readable markdown for a URL, or the active tab when omitted.
    Read { url: Option<String> },
    /// Screenshot to a path (temp dir when omitted).
    Screenshot { path: Option<String> },
    /// Save the page as PDF.
    Pdf { path: String },
    /// Run JavaScript in the page.
    Eval { js: String },
    /// Run JS and return raw output for the caller model to structure.
    Extract { js: String },
    /// Wait for a selector, text, URL, JS condition, or milliseconds.
    Wait { what: String },
    /// Cookie ops, passed through: cookies [set <n> <v> | clear].
    Cookies {
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Storage ops, passed through: storage <local|session> [set ...].
    Storage {
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Network ops, passed through: route / requests / har ...
    Network {
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Tab ops, passed through: tabs [new <url> | <t1|label> | close].
    Tabs {
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Dialog ops, passed through: dialog <accept|dismiss|status>.
    Dialog {
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Auth-state ops, passed through: state <save|load|list|...>.
    State {
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Multiple quoted commands in one invocation (one turn, one round-trip).
    Batch {
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        commands: Vec<String>,
    },
    /// Close the browser/session.
    Close,
    /// Diagnose the install via the sidecar's own doctor.
    Doctor,
    /// Local-only: backend presence + resolved session, no browser launch.
    Status,
}

impl BrowserArgs {
    pub fn run(self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let session = flare_browser::resolve_session(self.session.as_deref(), &cwd);
        if let BrowserCommands::Status = &self.command {
            return print_status(&session);
        }
        // (action, positionals, extra)
        let (action, positionals, extra): (&str, Vec<String>, Vec<String>) = match &self.command {
            BrowserCommands::Status => unreachable!(),
            BrowserCommands::Open { url } => ("open", vec![url.clone()], vec![]),
            BrowserCommands::Snapshot => ("snapshot", vec![], vec![]),
            BrowserCommands::Observe { .. } => ("snapshot", vec![], vec![]),
            BrowserCommands::Click { target } => ("click", vec![target.clone()], vec![]),
            BrowserCommands::Fill { target, text } => {
                ("fill", vec![target.clone(), text.clone()], vec![])
            }
            BrowserCommands::Type { target, text } => {
                ("type", vec![target.clone(), text.clone()], vec![])
            }
            BrowserCommands::Press { key } => ("press", vec![key.clone()], vec![]),
            BrowserCommands::Hover { target } => ("hover", vec![target.clone()], vec![]),
            BrowserCommands::Select { target, value } => {
                ("select", vec![target.clone(), value.clone()], vec![])
            }
            BrowserCommands::Check { target } => ("check", vec![target.clone()], vec![]),
            BrowserCommands::Uncheck { target } => ("uncheck", vec![target.clone()], vec![]),
            BrowserCommands::Back => ("back", vec![], vec![]),
            BrowserCommands::Forward => ("forward", vec![], vec![]),
            BrowserCommands::Reload => ("reload", vec![], vec![]),
            BrowserCommands::Get { kind, target } => {
                let mut p = vec![kind.clone()];
                if let Some(t) = target {
                    p.push(t.clone());
                }
                ("get", p, vec![])
            }
            BrowserCommands::Read { url } => ("read", url.clone().into_iter().collect(), vec![]),
            BrowserCommands::Screenshot { path } => {
                ("screenshot", path.clone().into_iter().collect(), vec![])
            }
            BrowserCommands::Pdf { path } => ("pdf", vec![path.clone()], vec![]),
            BrowserCommands::Eval { js } => ("eval", vec![js.clone()], vec![]),
            BrowserCommands::Extract { js } => ("extract", vec![js.clone()], vec![]),
            BrowserCommands::Wait { what } => ("wait", vec![what.clone()], vec![]),
            BrowserCommands::Cookies { args } => ("cookies", args.clone(), vec![]),
            BrowserCommands::Storage { args } => ("storage", args.clone(), vec![]),
            BrowserCommands::Network { args } => ("network", args.clone(), vec![]),
            BrowserCommands::Tabs { args } => ("tabs", args.clone(), vec![]),
            BrowserCommands::Dialog { args } => ("dialog", args.clone(), vec![]),
            BrowserCommands::State { args } => ("state", args.clone(), vec![]),
            BrowserCommands::Batch { commands } => ("batch", commands.clone(), vec![]),
            BrowserCommands::Close => ("close", vec![], vec![]),
            BrowserCommands::Doctor => ("doctor", vec!["--quick".to_string()], vec![]),
        };
        // Observe composes on snapshot output locally (no extra model call).
        let auto_install = !self.no_auto_install && flare_browser::auto_install_enabled();
        if let BrowserCommands::Observe { query, limit } = &self.command {
            return match exec_raw(&session, action, &positionals, &extra, auto_install) {
                Ok(snapshot) => println!(
                    "{}",
                    flare_browser::compact_output(
                        &flare_browser::observe_filter(&snapshot, query, *limit),
                        flare_browser::MAX_OUTPUT_CHARS,
                    )
                ),
                Err(e) => crate::ui::error(&e),
            };
        }
        match exec(&session, action, &positionals, &extra, auto_install) {
            Ok(out) => {
                if out.is_empty() {
                    crate::ui::success(&format!("browser {action} ok (session {session})"));
                } else {
                    println!("{out}");
                }
            }
            Err(e) => crate::ui::error(&e),
        }
    }
}

fn exec(
    session: &str,
    action: &str,
    positionals: &[String],
    extra: &[String],
    auto_install: bool,
) -> Result<String, String> {
    Ok(flare_browser::compact_output(
        &exec_raw(session, action, positionals, extra, auto_install)?,
        flare_browser::MAX_OUTPUT_CHARS,
    ))
}

/// Like [`exec`] but returns the sidecar's raw, untruncated output.
/// `observe` filters against the full snapshot before any truncation --
/// capping first (as `exec` does for direct display) can push a matching
/// line past the cut and silently miss it.
fn exec_raw(
    session: &str,
    action: &str,
    positionals: &[String],
    extra: &[String],
    auto_install: bool,
) -> Result<String, String> {
    if auto_install && flare_browser::find_backend().is_err() {
        println!(
            "agent-browser not found — installing via mise (prebuilt binary, one-time, usually under a minute)…"
        );
    }
    let backend = crate::browser_install::ensure_agent_browser(auto_install)?;
    let argv = flare_browser::build_argv(session, action, positionals, extra)?;
    flare_browser::run_blocking(&backend, &argv, &[])
}

fn print_status(session: &str) {
    println!("session: {session}");
    match flare_browser::find_backend() {
        Ok(path) => {
            println!(
                "backend: {} ({})",
                flare_browser::BACKEND_BIN,
                path.display()
            );
            let verbs: Vec<&str> = flare_browser::ACTIONS.iter().map(|a| a.name).collect();
            println!("actions: {}", verbs.join(", "));
            println!(
                "tip: override the session with --session <id> or ${}",
                flare_browser::SESSION_ENV
            );
        }
        Err(e) => crate::ui::error(&e),
    }
}
