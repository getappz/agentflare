use clap::{Args, Subcommand};
use std::path::PathBuf;

/// Hand a work product to another agent's inbox (publishes an artifact
/// with a handoff envelope; the recipient lists it with /flare:handoff inbox).
#[derive(Args)]
pub struct HandoffArgs {
    /// Target agent/runtime (e.g. opencode, claude-code, codex).
    pub recipient: Option<String>,
    /// File whose content to hand off.
    pub file: Option<PathBuf>,
    /// Inline content instead of a file.
    #[arg(long, conflicts_with = "file")]
    pub content: Option<String>,
    /// Thread id grouping an exchange (default: freshly generated).
    #[arg(long)]
    pub thread: Option<String>,
    /// Artifact id this replies to (reuse its thread via --thread).
    #[arg(long)]
    pub reply_to: Option<String>,
    /// Artifact name (default: file stem, or "handoff").
    #[arg(long)]
    pub name: Option<String>,
    /// Session id for grouping (default: handoffs).
    #[arg(long, default_value = "handoffs")]
    pub session: String,
    /// Sender identity (default: AGENTFLARE_AGENT, else the detected host agent, else "cli").
    #[arg(long)]
    pub sender: Option<String>,
    /// Storage directory (default: ~/.agentflare/artifacts).
    #[arg(long)]
    pub dir: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<HandoffCommands>,
}

/// Cross-agent continuity over agentflare's own store (item #674):
/// render/verify handoffs from read-only foreign-agent sessions.
/// Publish (no subcommand) keeps the original file|--content flow.
#[derive(Subcommand)]
pub enum HandoffCommands {
    /// Render the handoff markdown to stdout. Zero writes.
    Preview {
        /// Insights session id (`agentflare insights list`).
        session: String,
        /// Receiving agent/runtime.
        #[arg(long, default_value = "opencode")]
        target: String,
        /// minimal|standard|verbose|full (turn budget 3|10|20|50).
        #[arg(long, default_value = "standard")]
        verbosity: String,
        /// Insights DB (default: ~/.local/share/agentflare/insights/observatory.db).
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Render and write to a file (md|json), or stdout when --out is absent.
    Export {
        /// Insights session id.
        session: String,
        /// Receiving agent/runtime.
        #[arg(long, default_value = "opencode")]
        target: String,
        /// minimal|standard|verbose|full.
        #[arg(long, default_value = "standard")]
        verbosity: String,
        /// md|json.
        #[arg(long, default_value = "md")]
        format: String,
        /// Output file (default: stdout).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Insights DB (default: ~/.local/share/agentflare/insights/observatory.db).
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Pre-flight loss accounting: what would be carried vs dropped.
    Verify {
        /// Insights session id.
        session: String,
        /// Receiving agent/runtime.
        #[arg(long, default_value = "opencode")]
        target: String,
        /// Insights DB (default: ~/.local/share/agentflare/insights/observatory.db).
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Health of the handoff path: memory DB, insights DB, sources.
    Doctor {
        /// Insights DB (default: ~/.local/share/agentflare/insights/observatory.db).
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Load one foreign session read-only and publish it to an agent's inbox.
    Send {
        /// Foreign session id (see `agentflare insights list`).
        session: String,
        /// Source store: auto|claude_code|codex|opencode (aliases cc/claude/oc).
        #[arg(long, default_value = "auto")]
        source: String,
        /// Receiving agent/runtime.
        #[arg(long, default_value = "opencode")]
        target: String,
        /// minimal|standard|verbose|full.
        #[arg(long, default_value = "standard")]
        verbosity: String,
        /// Thread id grouping an exchange (default: freshly generated).
        #[arg(long)]
        thread: Option<String>,
        /// Artifact id this replies to.
        #[arg(long)]
        reply_to: Option<String>,
        /// Artifact name (default: handoff-<session>).
        #[arg(long)]
        name: Option<String>,
        /// Storage directory (default: ~/.agentflare/artifacts).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Materialize a handoff as a section in the target's instruction file.
    Apply {
        /// Foreign session id (see `agentflare insights list`).
        session: String,
        /// Source store: auto|claude_code|codex|opencode (aliases cc/claude/oc).
        #[arg(long, default_value = "auto")]
        source: String,
        /// Receiving agent/runtime (selects AGENTS.md vs CLAUDE.md).
        #[arg(long, default_value = "opencode")]
        target: String,
        /// minimal|standard|verbose|full.
        #[arg(long, default_value = "standard")]
        verbosity: String,
        /// Explicit instruction file (default: <cwd>/AGENTS.md or CLAUDE.md).
        #[arg(long)]
        file: Option<PathBuf>,
        /// Print the section without writing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Suggest (or execute) failover to the next agent when one is exhausted.
    Route {
        /// Exhausted agent (aliases cc/claude/oc accepted).
        from: String,
        /// Log excerpt / error text to classify.
        #[arg(long)]
        reason: Option<String>,
        /// Explicit target, skips auto-selection.
        #[arg(long)]
        to: Option<String>,
        /// Current chain depth (refuses at 5).
        #[arg(long, default_value = "0")]
        depth: u32,
        /// Perform the send, not just suggest (needs --session).
        #[arg(long)]
        execute: bool,
        /// Foreign session id (for --execute).
        #[arg(long)]
        session: Option<String>,
        /// minimal|standard|verbose|full (for --execute).
        #[arg(long, default_value = "standard")]
        verbosity: String,
    },
}

#[derive(Debug)]
pub struct HandoffOutcome {
    pub id: String,
    pub version: u32,
    pub thread_id: String,
}

impl HandoffArgs {
    pub fn run(self) {
        if let Some(cmd) = self.command {
            return run_continuity(cmd);
        }
        let Some(recipient) = self.recipient.clone() else {
            crate::ui::error(
                "missing recipient — pass an agent or a preview|export|verify|doctor subcommand",
            );
            std::process::exit(1);
        };
        match self.publish() {
            Ok(out) => {
                println!(
                    "Handed off artifact {} (v{}) to {recipient}",
                    out.id, out.version
                );
                println!("  thread: {}", out.thread_id);
                println!("  hint: {recipient} reads it via /flare:handoff inbox (or artifact_get)");
            }
            Err(e) => {
                crate::ui::error(&e.to_string());
                std::process::exit(1);
            }
        }
    }

    pub(crate) fn publish(self) -> Result<HandoffOutcome, String> {
        let (content, stem, ext) = match (&self.file, self.content) {
            (Some(path), None) => {
                let content = std::fs::read_to_string(path)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned());
                let ext = path.extension().map(|s| s.to_string_lossy().into_owned());
                (content, stem, ext)
            }
            (None, Some(content)) => (content, None, None),
            (Some(_), Some(_)) => return Err("pass a file or --content, not both".into()),
            (None, None) => return Err("nothing to hand off — pass a file or --content".into()),
        };

        let thread_id = self.thread.unwrap_or_else(|| {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            format!("t{nanos}")
        });
        // Routed through `claims::owner_id()` (which strips the `:instance`
        // suffix via `agent_of`) rather than reading `AGENTFLARE_AGENT`
        // directly: same fallback chain (env -> detected agent -> "cli") for
        // every existing caller, but it also picks up a thread-local
        // identity override for free when this runs as in-process dispatched
        // work inside the daemon (see `claims::with_owner_override`) —
        // env-var-based identity doesn't work there since worker threads
        // share one process env.
        let sender = self
            .sender
            .unwrap_or_else(|| crate::claims::agent_of(&crate::claims::owner_id()).to_string());

        let store: agentflare_artifacts::ArtifactStore = match self.dir.clone() {
            Some(d) => agentflare_artifacts::ArtifactStore::new(d),
            None => match crate::store::open() {
                Ok(s) => agentflare_artifacts::ArtifactStore::with_store(s),
                Err(e) => {
                    eprintln!("[handoff] fallback to flat-file store: {e}");
                    agentflare_artifacts::ArtifactStore::new(
                        crate::paths::agentflare_dir().join("artifacts"),
                    )
                }
            },
        };
        let req = agentflare_artifacts::PublishRequest {
            name: self.name.or(stem).unwrap_or_else(|| "handoff".into()),
            artifact_type: agentflare_artifacts::ArtifactType::from(
                ext.as_deref().unwrap_or("text"),
            ),
            content,
            session_id: self.session,
            update_id: None,
            label: None,
            description: None,
            favicon: Some("🤝".into()),
            base_version: None,
            sender: Some(sender),
            recipient: self.recipient,
            thread_id: Some(thread_id.clone()),
            reply_to: self.reply_to,
            git: crate::mcp_server::AgentflareMcp::git_provenance(),
        };
        let resp = store.publish(&req).map_err(|e| e.to_string())?;
        Ok(HandoffOutcome {
            id: resp.id,
            version: resp.version,
            thread_id,
        })
    }
}

fn run_continuity(cmd: HandoffCommands) {
    let out = match cmd {
        HandoffCommands::Preview {
            session,
            target,
            verbosity,
            db,
        } => crate::handoff::preview(db, &session, &target, &verbosity),
        HandoffCommands::Export {
            session,
            target,
            verbosity,
            format,
            out,
            db,
        } => crate::handoff::export_body(db, &session, &target, &verbosity, &format, out),
        HandoffCommands::Verify {
            session,
            target,
            db,
        } => crate::handoff::verify(db, &session, &target),
        HandoffCommands::Doctor { db } => crate::handoff::doctor(db),
        HandoffCommands::Send {
            session,
            source,
            target,
            verbosity,
            thread,
            reply_to,
            name,
            dir,
        } => crate::handoff::send(crate::handoff::SendRequest {
            source,
            session_id: session,
            target: target.clone(),
            verbosity,
            thread,
            reply_to,
            name,
            artifact_dir: dir,
        })
        .map(|out| {
            format!(
                "Handed off artifact {} (v{}) to {}\n  thread: {}\n  hint: {} reads it via /flare:handoff inbox (or artifact_get)",
                out.id, out.version, out.recipient, out.thread_id, out.recipient
            )
        }),
        HandoffCommands::Apply {
            session,
            source,
            target,
            verbosity,
            file,
            dry_run,
        } => {
            let preview = dry_run;
            crate::handoff::apply::apply(crate::handoff::apply::ApplyRequest {
                source,
                session_id: session,
                target,
                verbosity,
                file,
                dry_run,
            })
            .map(|out| {
                if out.wrote {
                    format!("applied continuity section to {}", out.path.display())
                } else if preview {
                    out.section
                } else {
                    format!("section already current in {}", out.path.display())
                }
            })
        }
        HandoffCommands::Route {
            from,
            reason,
            to,
            depth,
            execute,
            session,
            verbosity,
        } => crate::handoff::route::route(crate::handoff::route::RouteRequest {
            from,
            reason,
            to,
            depth,
            execute,
            session_id: session,
            verbosity,
        })
        .map(|out| {
            let mut text = format!(
                "from: {} (signal: {})\nrecommended: {}\nalternatives: {}\ndepth: {}",
                out.from,
                out.signal.as_deref().unwrap_or("unrecognized"),
                out.recommended.as_deref().unwrap_or("none"),
                out.alternatives.join(", "),
                out.depth,
            );
            if let Some(sent) = out.sent {
                text.push_str(&format!(
                    "\nsent artifact {} (v{}) to {} (thread {})",
                    sent.id, sent.version, sent.recipient, sent.thread_id
                ));
            } else {
                text.push_str("\nsuggestion only — re-run with --execute --session <id> to send");
            }
            text
        }),
    };
    match out {
        Ok(text) => println!("{text}"),
        Err(e) => {
            crate::ui::error(&e);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(dir: &std::path::Path) -> HandoffArgs {
        HandoffArgs {
            recipient: Some("opencode".into()),
            file: None,
            content: None,
            thread: None,
            reply_to: None,
            name: None,
            session: "handoffs".into(),
            sender: None,
            dir: Some(dir.to_path_buf()),
            command: None,
        }
    }

    #[test]
    fn handoff_publishes_file_with_envelope_and_type() {
        let tmp = tempfile::tempdir().unwrap();
        let note = tmp.path().join("review-notes.md");
        std::fs::write(&note, "# please review\nthe diff").unwrap();

        let out = HandoffArgs {
            file: Some(note),
            thread: Some("t-pr42".into()),
            sender: Some("claude-code".into()),
            ..args(tmp.path())
        }
        .publish()
        .unwrap();

        let store = agentflare_artifacts::ArtifactStore::new(tmp.path().to_path_buf());
        let artifact = store.get(&out.id).unwrap();
        assert_eq!(artifact.recipient.as_deref(), Some("opencode"));
        assert_eq!(artifact.sender.as_deref(), Some("claude-code"));
        assert_eq!(artifact.thread_id.as_deref(), Some("t-pr42"));
        assert_eq!(artifact.name, "review-notes");
        assert_eq!(
            artifact.artifact_type,
            agentflare_artifacts::ArtifactType::Markdown
        );
        assert!(artifact.content.contains("please review"));
    }

    #[test]
    fn handoff_inline_content_gets_generated_thread_and_sender() {
        let tmp = tempfile::tempdir().unwrap();
        let out = HandoffArgs {
            content: Some("review my changes".into()),
            ..args(tmp.path())
        }
        .publish()
        .unwrap();

        let store = agentflare_artifacts::ArtifactStore::new(tmp.path().to_path_buf());
        let artifact = store.get(&out.id).unwrap();
        assert!(artifact.thread_id.is_some_and(|t| !t.is_empty()));
        // Identity chain ends in "cli", so a sender always exists.
        assert!(artifact.sender.is_some_and(|s| !s.is_empty()));
        assert_eq!(artifact.name, "handoff");
    }

    #[test]
    fn handoff_requires_file_or_content() {
        let tmp = tempfile::tempdir().unwrap();
        let err = args(tmp.path()).publish().unwrap_err();
        assert!(err.contains("file or --content"), "{err}");
    }
}
