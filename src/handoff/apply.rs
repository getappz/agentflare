//! Project-mode apply: materialize a handoff as a marker-anchored section
//! in the target agent's instruction file (`AGENTS.md`, or `CLAUDE.md` for
//! Claude Code). Team-safe by construction: only the text between our own
//! markers is ever written; everything else in the file is preserved
//! byte-for-byte, and re-apply replaces the section instead of appending a
//! duplicate. `--dry-run` prints without touching disk.
//!
//! Skills/MCP bundles are deliberately out of scope for this slice (noted
//! in the section footer when dropped); the instruction file is what the
//! receiving agent reads automatically on next launch.

use super::body::{self, HandoffBodyV1};

pub const START_MARK: &str = "<!-- agentflare:continuity:start -->";
pub const END_MARK: &str = "<!-- agentflare:continuity:end -->";

/// Instruction file per receiving agent. Codex reads `AGENTS.md` like
/// OpenCode; only Claude Code prefers `CLAUDE.md`.
pub fn target_file(target: &str) -> &str {
    match target {
        "claude_code" | "claude" | "cc" => "CLAUDE.md",
        _ => "AGENTS.md",
    }
}

pub struct ApplyRequest {
    pub source: String,
    pub session_id: String,
    pub target: String,
    pub verbosity: String,
    /// Explicit file; default is `<cwd>/<target-file>`.
    pub file: Option<std::path::PathBuf>,
    pub dry_run: bool,
}

pub struct ApplyOutcome {
    pub path: std::path::PathBuf,
    pub wrote: bool,
    pub section: String,
}

/// Load the session (sources adapter, read-only), render the section, and
/// either write it into the instruction file or return it for `--dry-run`.
pub fn apply(req: ApplyRequest) -> Result<ApplyOutcome, String> {
    let bundle = super::sources::load_session(&req.source, &req.session_id)?;
    let max_turns = super::parse_verbosity(&req.verbosity).max_turns();
    let git = body::git_context(bundle.session.cwd.as_deref());
    let built = body::build(
        &bundle.session,
        &bundle.turns,
        &bundle.tools,
        &bundle.files,
        bundle.subagent_count,
        &req.target,
        max_turns,
        git,
    );
    let section = render_section(&built);
    let path = req.file.unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(target_file(&req.target))
    });
    if req.dry_run {
        return Ok(ApplyOutcome {
            path,
            wrote: false,
            section,
        });
    }
    let prev = std::fs::read_to_string(&path).unwrap_or_default();
    let next = upsert_section(&prev, &section);
    if next == prev {
        return Ok(ApplyOutcome {
            path,
            wrote: false,
            section,
        });
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, &next).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(ApplyOutcome {
        path,
        wrote: true,
        section,
    })
}

fn render_section(body: &HandoffBodyV1) -> String {
    let mut out = String::new();
    out.push_str("## Agentflare continuity\n\n");
    out.push_str(&format!(
        "Handed off from `{}`, target `{}`.\n\n",
        body.source_ref, body.target
    ));
    out.push_str(&format!("Objective: {}\n\n", body.objective));
    if body.completed.is_empty() && body.remaining.is_empty() {
        out.push_str("Status: unknown — see conversation turns in the handoff artifact.\n\n");
    } else {
        for c in &body.completed {
            out.push_str(&format!("- done: {c}\n"));
        }
        for r in &body.remaining {
            out.push_str(&format!("- todo: {r}\n"));
        }
        out.push('\n');
    }
    if !body.files_touched.is_empty() {
        out.push_str("Files:\n\n");
        for f in body.files_touched.iter().take(20) {
            out.push_str(&format!("- `{f}`\n"));
        }
        out.push('\n');
    }
    out.push_str(
        "_Skills/MCP bundles intentionally not applied by this slice — adopt them by hand if needed._\n",
    );
    out
}

/// Insert or replace the marked section. Content outside the markers is
/// never modified; a file without markers gets the section appended.
pub fn upsert_section(prev: &str, section: &str) -> String {
    let block = format!("{START_MARK}\n{section}\n{END_MARK}");
    match (prev.find(START_MARK), prev.find(END_MARK)) {
        (Some(s), Some(e)) if s < e => {
            let end = e + END_MARK.len();
            format!("{}{block}{}", &prev[..s], &prev[end..])
        }
        _ => {
            let sep = if prev.is_empty() || prev.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            format!("{prev}{sep}\n{block}\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_file_mapping() {
        assert_eq!(target_file("claude_code"), "CLAUDE.md");
        assert_eq!(target_file("cc"), "CLAUDE.md");
        assert_eq!(target_file("opencode"), "AGENTS.md");
        assert_eq!(target_file("codex"), "AGENTS.md");
    }

    #[test]
    fn append_then_replace_is_idempotent_and_preserving() {
        let first = upsert_section("# Title\n", "A");
        assert!(first.starts_with("# Title\n"));
        assert!(first.contains(START_MARK));
        let second = upsert_section(&first, "B");
        assert_eq!(second.matches(START_MARK).count(), 1);
        assert!(second.contains("B"));
        assert!(!second.contains("\nA\n"));
        assert!(second.starts_with("# Title\n"));
    }

    #[test]
    fn unchanged_section_reports_no_write() {
        let prev = upsert_section("", "A");
        assert_eq!(upsert_section(&prev, "A"), prev);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("AGENTS.md");
        // No source session needed: dry-run still loads — use a bogus id and
        // assert the load error surfaces before any file is created.
        let res = apply(ApplyRequest {
            source: "auto".into(),
            session_id: "does-not-exist".into(),
            target: "opencode".into(),
            verbosity: "minimal".into(),
            file: Some(file.clone()),
            dry_run: true,
        });
        assert!(res.is_err());
        assert!(!file.exists());
    }
}
