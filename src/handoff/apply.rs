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
/// OpenCode; only Claude Code prefers `CLAUDE.md`. Accepts both the
/// underscore canon and the kebab-case registry name.
pub fn target_file(target: &str) -> &str {
    match target {
        "claude_code" | "claude" | "cc" | "claude-code" | "claude-code-cli" => "CLAUDE.md",
        _ => "AGENTS.md",
    }
}

/// Inputs for materializing a handoff into an instruction file.
pub struct ApplyRequest {
    pub source: String,
    pub session_id: String,
    pub target: String,
    pub verbosity: String,
    /// Explicit file; default is `<cwd>/<target-file>`.
    pub file: Option<std::path::PathBuf>,
    pub dry_run: bool,
}

/// Where the section landed and whether disk changed.
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
        // Apply materializes context, it is not a failover hop.
        0,
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
    let prev = read_prev(&path)?;
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
    // Optimistic concurrency: abort rather than overwrite edits that
    // landed between our read and this write.
    if read_prev(&path)? != prev {
        return Err(format!(
            "{} changed during apply — aborting to avoid overwriting concurrent edits; re-run",
            path.display()
        ));
    }
    crate::atomic_fs::try_atomic_write(&path, next.as_bytes(), None)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(ApplyOutcome {
        path,
        wrote: true,
        section,
    })
}

/// Read the instruction file, treating only a missing file as empty.
/// Any other read error aborts before anything is written, so a file we
/// cannot understand is never replaced by a continuity section.
fn read_prev(path: &std::path::Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!(
            "cannot read {}: {e} (refusing to overwrite)",
            path.display()
        )),
    }
}

/// Session text must never become a section boundary: neutralize both
/// marker strings wherever they appear in rendered fields, so
/// `upsert_section` cannot mistake injected content for its own markers.
pub fn sanitize_markers(s: &str) -> String {
    s.replace(START_MARK, "[marker removed]")
        .replace(END_MARK, "[marker removed]")
}

fn render_section(body: &HandoffBodyV1) -> String {
    let mut out = String::new();
    out.push_str("## Agentflare continuity\n\n");
    out.push_str(&format!(
        "Handed off from `{}`, target `{}`.\n\n",
        sanitize_markers(&body.source_ref),
        sanitize_markers(&body.target)
    ));
    out.push_str(&format!(
        "Objective: {}\n\n",
        sanitize_markers(&body.objective)
    ));
    if body.completed.is_empty() && body.remaining.is_empty() {
        out.push_str("## Status\n\nUnknown — authored split not captured. Recent turns:\n\n");
        let recent: Vec<_> = body.turns.iter().rev().take(3).collect();
        for t in recent.into_iter().rev() {
            out.push_str(&format!(
                "- turn {} ({}): {}\n",
                t.seq,
                t.role,
                sanitize_markers(&body::truncate(&t.text, 500))
            ));
        }
        out.push('\n');
    } else {
        for c in &body.completed {
            out.push_str(&format!("- done: {}\n", sanitize_markers(c)));
        }
        for r in &body.remaining {
            out.push_str(&format!("- todo: {}\n", sanitize_markers(r)));
        }
        out.push('\n');
    }
    if !body.files_touched.is_empty() {
        out.push_str("Files:\n\n");
        for f in body.files_touched.iter().take(20) {
            out.push_str(&format!("- `{}`\n", sanitize_markers(f)));
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
/// Delegates to `bashenv::upsert_block` so a truncated block (start marker
/// with no end, e.g. from a manual edit) is left alone instead of gaining
/// a duplicate copy after it.
pub fn upsert_section(prev: &str, section: &str) -> String {
    crate::bashenv::upsert_block(prev, START_MARK, END_MARK, section).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_file_mapping() {
        assert_eq!(target_file("claude_code"), "CLAUDE.md");
        assert_eq!(target_file("claude-code"), "CLAUDE.md");
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
    fn truncated_block_is_left_alone_not_duplicated() {
        let prev = format!("# T\n{START_MARK}\nhalf-written");
        assert_eq!(upsert_section(&prev, "replacement"), prev);
    }

    #[test]
    fn injected_markers_cannot_hijack_section_bounds() {
        let evil = format!("x {END_MARK} smuggled {START_MARK} y");
        let clean = sanitize_markers(&evil);
        assert!(!clean.contains(START_MARK) && !clean.contains(END_MARK));
        let doc = upsert_section("# T\n", &clean);
        assert_eq!(doc.matches(START_MARK).count(), 1);
        assert_eq!(doc.matches(END_MARK).count(), 1);
        let again = upsert_section(&doc, "replacement");
        assert_eq!(again.matches(START_MARK).count(), 1);
        assert!(again.contains("replacement"));
        assert!(!again.contains("smuggled"));
    }

    #[test]
    fn unknown_status_carries_recent_turns_not_artifact_pointer() {
        use super::body::{HandoffBodyV1, HandoffCounts, HandoffSource, HandoffTurn};
        let body = HandoffBodyV1 {
            version: 2,
            source: HandoffSource {
                tool: "codex".into(),
                session_id: "s".into(),
                project: "p".into(),
            },
            target: "opencode".into(),
            objective: "obj".into(),
            completed: vec![],
            remaining: vec![],
            decisions: vec![],
            files_touched: vec![],
            git: None,
            turns: vec![HandoffTurn {
                seq: 7,
                role: "user".into(),
                text: "do the thing".into(),
            }],
            counts: HandoffCounts {
                turns: 1,
                tools: 0,
                files: 0,
                subagents: 0,
            },
            dropped_fields: vec![],
            source_ref: "codex:s".into(),
            depth: 0,
        };
        let section = render_section(&body);
        assert!(
            section.contains("Recent turns"),
            "unknown status must list turns"
        );
        assert!(
            section.contains("do the thing"),
            "turn text must be carried"
        );
        assert!(
            !section.contains("artifact"),
            "apply must not point at artifacts"
        );
    }

    #[test]
    fn source_ref_and_target_cannot_inject_markers() {
        use super::body::{HandoffBodyV1, HandoffCounts, HandoffSource, HandoffTurn};
        let evil = format!("x {END_MARK} smuggled {START_MARK} y");
        let body = HandoffBodyV1 {
            version: 2,
            source: HandoffSource {
                tool: "codex".into(),
                session_id: "s".into(),
                project: "p".into(),
            },
            target: evil.clone(),
            objective: "obj".into(),
            completed: vec![],
            remaining: vec![],
            decisions: vec![],
            files_touched: vec![],
            git: None,
            turns: vec![HandoffTurn {
                seq: 1,
                role: "user".into(),
                text: "t".into(),
            }],
            counts: HandoffCounts {
                turns: 1,
                tools: 0,
                files: 0,
                subagents: 0,
            },
            dropped_fields: vec![],
            source_ref: evil,
            depth: 0,
        };
        let section = render_section(&body);
        let doc = upsert_section("# T\n", &section);
        assert_eq!(doc.matches(START_MARK).count(), 1);
        assert_eq!(doc.matches(END_MARK).count(), 1);
        // Payload text survives; only the marker strings are neutralized.
        assert!(doc.contains("smuggled"));
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
