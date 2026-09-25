//! Cross-agent handoff body, schema v1.
//!
//! Pure builder + renderer over `flare_insights` session parts. No I/O and
//! no writes to foreign stores: the caller (CLI `handoff preview/export`,
//! MCP later) fetches parts read-only from the insights DB and publishes
//! through agentflare's own artifact store. Anything this layer cannot know
//! (completed-vs-remaining split, decisions, subagent transcripts, raw tool
//! output) is listed in `dropped_fields` instead of being fabricated —
//! loss accounting, not silent loss.
//!
//! Home-path scrubbing reuses `agentflare-approval::redact::scrub_paths`
//! (pattern-based, env-independent); secret masking stays local because that
//! scrubber is command-oriented (space-split tokens, 512-char cap) while
//! turn text needs whole-prose masking without a length cap.

use flare_insights::model::{FileEvent, Session, ToolCall, Turn};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 2;
/// Per-turn-text cap, matching `flare_insights::handoff` truncation.
pub const MAX_TURN_CHARS: usize = 4000;
/// Objective fallback cap for the first-user-turn excerpt.
const MAX_OBJECTIVE_CHARS: usize = 280;

/// Secret-shaped token prefixes worth masking outright, mirroring
/// `agentflare-approval::redact::SECRET_PREFIXES`.
const SECRET_PREFIXES: &[&str] = &[
    "sk-", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "xox", "AKIA", "AIza",
];

/// Flag/env-var names whose `key=value` value must never be carried
/// verbatim, mirroring `agentflare-approval`'s sensitive flag list. Only the
/// `key=value` shape is masked (never bare prose words): an `=` signals an
/// assignment, so `password=hunter2` is caught while "the token expires"
/// passes through untouched.
const SENSITIVE_NAMES: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "token",
    "secret",
    "api_key",
    "apikey",
    "auth",
    "authorization",
    "access_key",
    "access_token",
    "private_key",
    "client_secret",
];

/// Origin of a handoff: which tool's store the session came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffSource {
    pub tool: String,
    pub session_id: String,
    pub project: String,
}

/// One flattened conversation turn (user or assistant side).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffTurn {
    pub seq: u32,
    pub role: String,
    pub text: String,
}

/// Best-effort repo state at handoff time; `None` when unresolvable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffGit {
    pub root: String,
    pub branch: String,
    pub commit: String,
    pub dirty_count: usize,
}

/// Full-fidelity counts behind the (possibly truncated) carried text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffCounts {
    pub turns: usize,
    pub tools: usize,
    pub files: usize,
    pub subagents: usize,
}

/// Versioned handoff payload: everything a receiving agent needs to
/// continue, plus an explicit list of what was knowingly dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffBodyV1 {
    pub version: u32,
    pub source: HandoffSource,
    pub target: String,
    pub objective: String,
    pub completed: Vec<String>,
    pub remaining: Vec<String>,
    pub decisions: Vec<String>,
    pub files_touched: Vec<String>,
    pub git: Option<HandoffGit>,
    pub turns: Vec<HandoffTurn>,
    pub counts: HandoffCounts,
    pub dropped_fields: Vec<String>,
    pub source_ref: String,
    /// Failover chain depth at send time (v2): the receiver passes it back
    /// as `--depth` so the cap survives across handoffs instead of
    /// resetting to 0 at every hop.
    pub depth: u32,
}

/// Build a v1 body from already-fetched session parts.
///
/// `max_turns` caps turns recency-first (newest kept). `target` names the
/// receiving agent. Nothing is read from disk here; pass `git: None` when
/// the caller has no repo context.
#[allow(clippy::too_many_arguments)]
pub fn build(
    session: &Session,
    turns: &[Turn],
    tools: &[ToolCall],
    files: &[FileEvent],
    subagent_count: usize,
    target: &str,
    max_turns: usize,
    git: Option<HandoffGit>,
    depth: u32,
) -> HandoffBodyV1 {
    let objective = objective_of(session, turns);
    let files_touched = files_touched_of(files);
    let handoff_turns = handoff_turns_of(turns, max_turns);

    let mut dropped = vec![
        "completed/remaining split: author at capture time (this layer only sees turns)".into(),
        "decisions: not inferable from turns; capture via memory handoff instead".into(),
        "subagent transcripts: counts only".into(),
        "tool call inputs/outputs: counts only (use --full export when it exists)".into(),
    ];
    if files_touched.is_empty() {
        dropped.push("files_touched: no write/edit file events".into());
    }
    if turns.len() > max_turns {
        dropped.push(format!(
            "oldest {} turns omitted (verbosity cap {max_turns}; see counts)",
            turns.len() - max_turns
        ));
    }
    if turns_truncated(turns) {
        dropped.push(format!("turn text truncated to {MAX_TURN_CHARS} chars"));
    }
    if git.is_none() {
        dropped.push("git context: no repo resolvable from session cwd".into());
    }

    HandoffBodyV1 {
        version: SCHEMA_VERSION,
        source: HandoffSource {
            tool: session.source.as_str().to_string(),
            session_id: session.id.clone(),
            project: session.project.clone(),
        },
        target: target.to_string(),
        objective,
        completed: Vec::new(),
        remaining: Vec::new(),
        decisions: Vec::new(),
        files_touched,
        git,
        turns: handoff_turns,
        counts: HandoffCounts {
            turns: turns.len(),
            tools: tools.len(),
            files: files.len(),
            subagents: subagent_count,
        },
        dropped_fields: dropped,
        source_ref: format!("{}:{}", session.source.as_str(), session.id),
        depth,
    }
}

fn objective_of(session: &Session, turns: &[Turn]) -> String {
    if let Some(title) = session.title.as_deref().filter(|t| !t.trim().is_empty()) {
        return redact(&truncate(title.trim(), MAX_OBJECTIVE_CHARS));
    }
    for t in turns {
        if let Some(u) = t.user_text.as_deref().filter(|u| !u.trim().is_empty()) {
            return redact(&truncate(u.trim(), MAX_OBJECTIVE_CHARS));
        }
    }
    format!("continue session {}", session.id)
}

fn files_touched_of(files: &[FileEvent]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in files {
        if (f.kind == "write" || f.kind == "edit") && !out.contains(&f.path) {
            out.push(f.path.clone());
        }
    }
    out
}

/// True when any carried turn exceeds the per-turn cap, i.e. the loss
/// accounting must note truncation.
fn turns_truncated(turns: &[Turn]) -> bool {
    turns.iter().any(|t| {
        t.user_text
            .as_deref()
            .is_some_and(|u| u.trim().chars().count() > MAX_TURN_CHARS)
            || t.assistant_text
                .as_deref()
                .is_some_and(|a| a.trim().chars().count() > MAX_TURN_CHARS)
    })
}

fn handoff_turns_of(turns: &[Turn], max_turns: usize) -> Vec<HandoffTurn> {
    let start = turns.len().saturating_sub(max_turns);
    let mut out = Vec::new();
    for t in &turns[start..] {
        if let Some(u) = t.user_text.as_deref().filter(|u| !u.trim().is_empty()) {
            out.push(HandoffTurn {
                seq: t.seq,
                role: "user".into(),
                text: redact(&truncate(u.trim(), MAX_TURN_CHARS)),
            });
        }
        if let Some(a) = t.assistant_text.as_deref().filter(|a| !a.trim().is_empty()) {
            out.push(HandoffTurn {
                seq: t.seq,
                role: "assistant".into(),
                text: redact(&truncate(a.trim(), MAX_TURN_CHARS)),
            });
        }
    }
    out
}

/// Render the body as the markdown handoff the receiving agent pastes in.
pub fn render_markdown(body: &HandoffBodyV1) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Handoff: {} → {}\n\n",
        body.source.session_id, body.target
    ));
    out.push_str(&format!(
        "Source: {} | Project: {} | Model: see source session\n\n",
        body.source.tool, body.source.project
    ));
    out.push_str(&format!("## Objective\n\n{}\n\n", body.objective));
    if body.completed.is_empty() && body.remaining.is_empty() {
        out.push_str(
            "## Status\n\nUnknown — authored split not captured; infer from turns below.\n\n",
        );
    } else {
        if !body.completed.is_empty() {
            out.push_str("## Completed\n\n");
            for c in &body.completed {
                out.push_str(&format!("- {c}\n"));
            }
            out.push('\n');
        }
        if !body.remaining.is_empty() {
            out.push_str("## Remaining\n\n");
            for r in &body.remaining {
                out.push_str(&format!("- {r}\n"));
            }
            out.push('\n');
        }
    }
    if !body.files_touched.is_empty() {
        out.push_str("## Files touched\n\n");
        for f in &body.files_touched {
            out.push_str(&format!("- `{f}`\n"));
        }
        out.push('\n');
    }
    if let Some(g) = &body.git {
        out.push_str(&format!(
            "## Git\n\nroot: `{}` | branch: `{}` | commit: `{}` | dirty files: {}\n\n",
            g.root, g.branch, g.commit, g.dirty_count
        ));
    }
    out.push_str(&format!(
        "## Conversation (last {}, {} total turns)\n\n",
        body.turns.len(),
        body.counts.turns
    ));
    for t in &body.turns {
        out.push_str(&format!(
            "### Turn {} ({})\n\n{}\n\n",
            t.seq, t.role, t.text
        ));
    }
    out.push_str("## Appendix\n\n");
    out.push_str(&format!(
        "counts: turns={} tools={} files={} subagents={} | failover depth: {}\n\n",
        body.counts.turns, body.counts.tools, body.counts.files, body.counts.subagents, body.depth
    ));
    out.push_str("dropped (not carried over):\n\n");
    for d in &body.dropped_fields {
        out.push_str(&format!("- {d}\n"));
    }
    out.push_str(&format!(
        "\n---\nContinue this session in `{}` by pasting this context. Full source: `{}`.\n",
        body.target, body.source_ref
    ));
    out
}

/// Best-effort git context for a session working directory. Every step
/// bails to `None`: a handoff must never fail because git is absent.
/// Shells out through `flare-git-core` (safe git resolution, no shim
/// recursion) instead of a hand-rolled `Command`.
pub fn git_context(cwd: Option<&str>) -> Option<HandoffGit> {
    use flare_git_core::shell::run_in_opt;
    let cwd = cwd.filter(|c| !c.is_empty())?;
    let dir = std::path::Path::new(cwd);
    let root = run_in_opt(dir, &["rev-parse", "--show-toplevel"])?;
    let branch = run_in_opt(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let commit = run_in_opt(dir, &["rev-parse", "--short", "HEAD"])?;
    // A failed status probe must not fabricate "0 dirty files": bail the
    // whole context instead (the caller records the loss in dropped_fields).
    let dirty_count = run_in_opt(dir, &["status", "--porcelain"]).map(|s| s.lines().count())?;
    Some(HandoffGit {
        root,
        branch,
        commit,
        dirty_count,
    })
}

/// Whole-prose secret scrubber: home-path scrubbing, secret-prefix word
/// masking, sensitive `key=value` masking, and private-key block masking.
/// No length cap — truncation is the caller's job (`truncate` before `redact`).
pub fn redact(input: &str) -> String {
    let folded = agentflare_approval::redact::scrub_paths(input);
    let unkeyed = mask_key_blocks(&folded);
    mask_secret_words(&unkeyed)
}

fn mask_key_blocks(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("-----BEGIN") {
        // Judge only the current block: the header line, or up to the next
        // END when present. Probing the whole remainder mistakes later prose
        // mentioning "PRIVATE KEY" for a key block and drops real context.
        let block_end = rest[start..].find("-----END").map(|rel| start + rel);
        let header_end = rest[start..]
            .find('\n')
            .map(|rel| start + rel)
            .unwrap_or(rest.len());
        if rest[start..block_end.unwrap_or(header_end)].contains("PRIVATE KEY") {
            out.push_str(&rest[..start]);
            out.push_str("[REDACTED PRIVATE KEY]");
            match rest[start..].find("-----END") {
                Some(rel) => {
                    let after = &rest[start + rel..];
                    match after.find('\n') {
                        Some(nl) => rest = &after[nl + 1..],
                        None => {
                            rest = "";
                            break;
                        }
                    }
                }
                None => {
                    rest = "";
                    break;
                }
            }
        } else {
            out.push_str(&rest[..start + "-----BEGIN".len()]);
            rest = &rest[start + "-----BEGIN".len()..];
        }
    }
    out.push_str(rest);
    out
}

/// Word-based secret masking with boundary-aware prefix matching: a
/// prefix only counts at the start of the word or after a non-alphanumeric
/// character, so ordinary words like `task-list` or `disk-based` survive
/// while `KEY=sk-...` and `"ghp_..."` are still caught.
fn mask_secret_words(input: &str) -> String {
    input
        .split_inclusive(|c: char| c.is_whitespace())
        .map(|tok| {
            let word = tok.trim_end();
            let tail = &tok[word.len()..];
            let suspicious = SECRET_PREFIXES.iter().any(|p| {
                word.match_indices(p).any(|(i, _)| {
                    let at_boundary = word[..i]
                        .chars()
                        .next_back()
                        .is_none_or(|c| !c.is_alphanumeric());
                    at_boundary && word.len() - i > p.len() + 4
                })
            });
            if suspicious || has_sensitive_name(word) {
                format!("[REDACTED]{tail}")
            } else {
                tok.to_string()
            }
        })
        .collect()
}

/// True for `key=value` words whose key is a known secret name
/// (`password=hunter2`, `--api-key=sk-...`). Bare prose words never match:
/// without an `=` there is no assignment to mask.
fn has_sensitive_name(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let bare = name
        .trim()
        .trim_start_matches('-')
        .trim_matches(|c| c == '"' || c == '\'')
        .to_ascii_lowercase();
    SENSITIVE_NAMES.contains(&bare.as_str())
}

/// Char-boundary truncation with an ellipsis marker when cut.
pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use flare_insights::model::{SessionSource, SessionStatus, TokenUsage};

    fn session() -> Session {
        Session {
            id: "ses-test".into(),
            source: SessionSource::ClaudeCode,
            project: "demo".into(),
            project_path: None,
            title: None,
            model: Some("opus".into()),
            status: SessionStatus::Active,
            awaiting_reason: None,
            started_at: None,
            updated_at: Utc::now(),
            ended_at: None,
            duration_secs: None,
            tokens: TokenUsage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost: None,
            turn_count: 2,
            tool_call_count: 1,
            subagent_count: 0,
            tags: vec![],
            starred: false,
            pid: None,
            cwd: None,
        }
    }

    fn turn(seq: u32, user: Option<&str>, assistant: Option<&str>) -> Turn {
        Turn {
            id: format!("t{seq}"),
            session_id: "ses-test".into(),
            seq,
            user_text: user.map(str::to_string),
            assistant_text: assistant.map(str::to_string),
            started_at: None,
            ended_at: None,
            tokens: None,
            cost_usd: None,
        }
    }

    #[test]
    fn objective_falls_back_to_first_user_turn() {
        let turns = vec![turn(1, Some("fix the login bug"), Some("on it"))];
        let b = build(&session(), &turns, &[], &[], 0, "opencode", 10, None, 0);
        assert_eq!(b.objective, "fix the login bug");
        assert_eq!(b.version, SCHEMA_VERSION);
        assert_eq!(b.turns.len(), 2);
        assert!(!b.dropped_fields.is_empty());
    }

    #[test]
    fn title_wins_over_turns_for_objective() {
        let mut s = session();
        s.title = Some("auth refactor".into());
        let turns = vec![turn(1, Some("something else"), None)];
        let b = build(&s, &turns, &[], &[], 0, "opencode", 10, None, 0);
        assert_eq!(b.objective, "auth refactor");
    }

    #[test]
    fn turns_cap_is_recency_first() {
        let turns: Vec<Turn> = (1..=5)
            .map(|i| turn(i, Some(&format!("q{i}")), None))
            .collect();
        let b = build(&session(), &turns, &[], &[], 0, "opencode", 2, None, 0);
        let seqs: Vec<u32> = b.turns.iter().map(|t| t.seq).collect();
        assert_eq!(seqs, vec![4, 5]);
        assert_eq!(b.counts.turns, 5);
    }

    #[test]
    fn dropped_fields_record_turn_cap_and_truncation() {
        let turns: Vec<Turn> = (1..=5)
            .map(|i| turn(i, Some(&format!("q{i}")), None))
            .collect();
        let b = build(&session(), &turns, &[], &[], 0, "opencode", 2, None, 0);
        assert!(
            b.dropped_fields
                .iter()
                .any(|d| d.contains("oldest 3 turns omitted"))
        );
        let long = "x".repeat(MAX_TURN_CHARS + 1);
        let turns = vec![turn(1, Some(long.as_str()), None)];
        let b = build(&session(), &turns, &[], &[], 0, "opencode", 10, None, 0);
        assert!(b.dropped_fields.iter().any(|d| d.contains("truncated")));
        let turns = vec![turn(1, Some("q"), None)];
        let b = build(&session(), &turns, &[], &[], 0, "opencode", 10, None, 0);
        assert!(
            b.dropped_fields
                .iter()
                .all(|d| !d.contains("omitted") && !d.contains("truncated"))
        );
    }

    #[test]
    fn secrets_are_masked_not_carried() {
        let out = redact("call with sk-ant-abcdefghij done");
        assert!(out.contains("[REDACTED]"));
        assert!(!out.contains("sk-ant-abcdefghij"));
    }

    #[test]
    fn private_key_blocks_are_dropped() {
        let key =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIBsecret\n-----END RSA PRIVATE KEY-----\nafter";
        let out = redact(key);
        assert!(!out.contains("MIIBsecret"));
        assert!(out.contains("[REDACTED PRIVATE KEY]"));
        assert!(out.contains("after"));
    }

    #[test]
    fn non_key_blocks_survive_later_prose() {
        let input = "-----BEGIN CERTIFICATE-----\nMIIBcert\n-----END CERTIFICATE-----\nnote about PRIVATE KEY handling";
        let out = redact(input);
        assert!(out.contains("MIIBcert"), "non-key blocks must survive");
        assert!(
            !out.contains("[REDACTED PRIVATE KEY]"),
            "prose mentioning keys must not trigger redaction"
        );
    }

    #[test]
    fn sensitive_key_values_masked_prose_untouched() {
        let out = redact("deploy with password=hunter2 now");
        assert!(!out.contains("hunter2"), "secret value must be masked");
        assert!(
            out.contains("[REDACTED]"),
            "secret assignment must be masked"
        );
        let out = redact("the token expires soon");
        assert!(!out.contains("[REDACTED]"), "bare prose must survive");
    }

    #[test]
    fn home_paths_scrubbed_without_env() {
        let out = redact("edit /Users/bob/proj/main.rs now");
        assert!(!out.contains("bob"), "home path must be scrubbed");
    }

    #[test]
    fn ordinary_words_survive_masking() {
        let out = redact("fix task-list and disk-based risk-free builds");
        assert!(
            !out.contains("[REDACTED]"),
            "ordinary words must survive masking"
        );
    }

    #[test]
    fn prefixed_secrets_still_masked() {
        assert!(redact("KEY=sk-ant-abcdefghij").contains("[REDACTED]"));
        assert!(redact("\"ghp_1234567890abcdef\"").contains("[REDACTED]"));
    }

    #[test]
    fn objective_is_redacted() {
        let turns = vec![turn(1, Some("deploy with key sk-ant-abcdefghij now"), None)];
        let b = build(&session(), &turns, &[], &[], 0, "opencode", 10, None, 0);
        assert!(
            !b.objective.contains("sk-ant-abcdefghij"),
            "objective must not carry raw secrets"
        );
    }

    #[test]
    fn markdown_renders_all_sections() {
        let turns = vec![turn(1, Some("do x"), Some("did x"))];
        let b = build(&session(), &turns, &[], &[], 0, "codex", 10, None, 0);
        let md = render_markdown(&b);
        assert!(md.contains("# Handoff: ses-test → codex"));
        assert!(md.contains("## Objective"));
        assert!(md.contains("## Conversation"));
        assert!(md.contains("## Appendix"));
        assert!(md.contains("dropped (not carried over)"));
    }
}
