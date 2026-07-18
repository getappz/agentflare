use clap::Args;
use std::path::PathBuf;

/// Hand a work product to another agent's inbox (publishes an artifact
/// with a handoff envelope; the recipient lists it with /flare:handoff inbox).
#[derive(Args)]
pub struct HandoffArgs {
    /// Target agent/runtime (e.g. opencode, claude-code, codex).
    pub recipient: String,
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
}

#[derive(Debug)]
pub struct HandoffOutcome {
    pub id: String,
    pub version: u32,
    pub thread_id: String,
}

impl HandoffArgs {
    pub fn run(self) {
        let recipient = self.recipient.clone();
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

    fn publish(self) -> Result<HandoffOutcome, String> {
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
        let sender = self
            .sender
            .or_else(|| {
                std::env::var("AGENTFLARE_AGENT")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
            .or_else(agent_detector::agent_name)
            .unwrap_or_else(|| "cli".into());

        let dir = self
            .dir
            .unwrap_or_else(|| crate::paths::home().join(".agentflare").join("artifacts"));
        let store = agentflare_artifacts::ArtifactStore::new(dir);
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
            recipient: Some(self.recipient),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(dir: &std::path::Path) -> HandoffArgs {
        HandoffArgs {
            recipient: "opencode".into(),
            file: None,
            content: None,
            thread: None,
            reply_to: None,
            name: None,
            session: "handoffs".into(),
            sender: None,
            dir: Some(dir.to_path_buf()),
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
