//! Denial-to-proposal mapper + containment check, ported from OpenShell's
//! `mechanistic_mapper` (`openshell-sandbox/src/mechanistic_mapper.rs`) and
//! the prover's containment question.
//!
//! OpenShell turns `DenialSummary(host, port, binary)` groups into draft
//! `PolicyChunk` proposals with confidence scores and security notes, instead
//! of hand-maintaining per-agent path tables. The sandbox here fails the same
//! way: a job dies with `EROFS`/`ENOENT` under the read-only root (items
//! #127, #106, #130, #120, #236, #241) and a human adds a hardcoded mount.
//! This module generalizes that loop: group denials by
//! `(suggested $HOME dir, binary)`, propose the minimal writable dir, and
//! check the proposal against the active boundary before applying it.
//!
//! The containment check answers one question: is candidate dir `C` already
//! covered by boundary `B` (exact match or `B` is a parent of `C`)? Adding a
//! covered dir is a no-op; adding anything else exceeds the boundary and
//! needs an explicit config change (`SandboxConfig.writable_home_dirs` or
//! `AGENTFLARE_SANDBOX_WRITABLE_HOME_DIRS`).

use std::collections::HashMap;

/// Why a sandboxed job failed to reach a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DenialKind {
    /// Read-only filesystem (bwrap read-only root): needs a writable mount.
    ReadOnlyFs,
    /// Missing file/dir the agent creates on demand: needs a tmpfs or mount.
    NotFound,
}

/// One observed sandbox denial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    /// Binary that hit the denial, e.g. `cursor-agent`.
    pub binary: String,
    /// Absolute path that was denied, e.g. `/home/u/.cursor/projects/x`.
    pub path: String,
    /// Failure mode.
    pub kind: DenialKind,
    /// How many times this denial was observed (aggregator hit count).
    pub count: u32,
}

/// A draft mount proposal for a denial group.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    /// Deterministic rule name, e.g. `mount__cursor`.
    pub rule_name: String,
    /// `$HOME`-relative dir to add to `writable_home_dirs`, e.g. `.cursor`.
    pub suggested_dir: String,
    /// Human-readable rationale (no secrets, paths are `$HOME`-relative).
    pub rationale: String,
    /// 0.1..=0.95 confidence, as in OpenShell's `compute_confidence`.
    pub confidence: f32,
    /// Warning when the dir hosts secrets (`.ssh`, `.aws`, `.gnupg`).
    pub security_note: Option<String>,
    /// True for secret-hosting dirs: never auto-apply, needs a human to
    /// confirm (the local analogue of OpenShell's prover-gated approval:
    /// any finding blocks auto-approval).
    pub needs_explicit_approval: bool,
}

/// Containment verdict for a candidate dir against the active boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Containment {
    /// Already covered: adding it changes nothing.
    Within,
    /// Not covered: exceeds the boundary, needs explicit approval.
    Exceeds,
}

/// `$HOME`-relative writable dirs this crate's callers already know about;
/// proposals for them score higher (repeatable, previously reviewed).
const WELL_KNOWN_DIRS: &[&str] = &[
    ".agentflare",
    ".local/share/lean-ctx",
    ".cache/gh",
    ".claude",
    ".config/opencode",
    ".local/share/opencode",
    ".cursor",
    ".config/cursor",
    ".codex",
    ".gemini",
    ".aider",
    ".grok",
    ".kimi-code",
];

/// Dirs that host long-lived secrets: proposing them gets a security note
/// and requires explicit human approval (never auto-applied).
const SECRET_DIRS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".pki",
    ".config/gh",
    ".config/gcloud",
    ".docker",
];

/// A `$HOME`-relative candidate is valid when it is non-empty, relative, and
/// has no empty/`.`/`..` components (same rule as `paths::is_valid_relative`,
/// duplicated here to keep this module dependency-free for callers).
#[must_use]
pub fn is_valid_writable_dir(candidate: &str) -> bool {
    crate::paths::is_valid_relative(candidate)
}

/// Suggest the minimal `$HOME`-relative writable dir for an absolute denied
/// path under `home`. Multi-level state prefixes keep their depth
/// (`.local/share/opencode`, `.config/cursor`); other dot-dirs collapse to
/// their top level (`.cursor/projects/x` -> `.cursor`). Returns `None` when
/// the path is not under `home` or yields no valid dir.
#[must_use]
pub fn suggest_writable_dir(path: &str, home: &str) -> Option<String> {
    let home = home.trim_end_matches('/');
    let relative = path.strip_prefix(home)?.strip_prefix('/')?;
    if relative.is_empty() {
        return None;
    }
    let parts: Vec<&str> = relative.split('/').collect();
    let depth = if relative.starts_with(".local/share/") {
        3
    } else if relative.starts_with(".config/") {
        2
    } else {
        1
    };
    let take = depth.min(parts.len());
    let candidate = parts[..take].join("/");
    if !is_valid_writable_dir(&candidate) {
        return None;
    }
    Some(candidate)
}

/// Group denials by `(suggested dir, binary)` and propose one mount per
/// group, highest confidence first. Denials with no suggestion (outside
/// `$HOME`, always-blocked dotfiles like `.ssh` are still proposed but with
/// a security note) are skipped; an empty input yields no proposals.
#[must_use]
pub fn generate_proposals(denials: &[Denial], home: &str) -> Vec<Proposal> {
    let mut groups: HashMap<(String, String), u32> = HashMap::new();
    for denial in denials {
        if denial.kind != DenialKind::ReadOnlyFs && denial.kind != DenialKind::NotFound {
            continue;
        }
        let Some(dir) = suggest_writable_dir(&denial.path, home) else {
            continue;
        };
        *groups.entry((dir, denial.binary.clone())).or_default() += denial.count.max(1);
    }

    let mut proposals: Vec<Proposal> = groups
        .into_iter()
        .map(|((dir, binary), count)| {
            let well_known = WELL_KNOWN_DIRS.contains(&dir.as_str());
            let confidence = compute_confidence(count, well_known);
            let rule_name = generate_rule_name(&dir);
            let rationale = format!(
                "Allow {binary} to write ${{HOME}}/{dir} (observed read-only-root denial)."
            );
            let secret = is_secret_dir(&dir);
            let security_note = secret.then(|| {
                "Hosts long-lived secrets; prefer an ephemeral overlay over a persistent writable bind.".to_string()
            });
            Proposal {
                rule_name,
                suggested_dir: dir,
                rationale,
                confidence,
                security_note,
                needs_explicit_approval: secret,
            }
        })
        .collect();
    proposals.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    proposals
}

/// Check `candidate` (`$HOME`-relative) against `boundary` (the active
/// `writable_home_dirs`): [`Containment::Within`] when the boundary already
/// covers it exactly or via a parent entry, [`Containment::Exceeds`]
/// otherwise. Invalid candidates always exceed.
#[must_use]
pub fn check_within_boundary(candidate: &str, boundary: &[String]) -> Containment {
    if !is_valid_writable_dir(candidate) {
        return Containment::Exceeds;
    }
    if boundary.iter().any(|b| covers(b, candidate)) {
        Containment::Within
    } else {
        Containment::Exceeds
    }
}

fn covers(entry: &str, candidate: &str) -> bool {
    let entry = entry.trim().trim_end_matches('/');
    if entry.is_empty() || !is_valid_writable_dir(entry) {
        return false;
    }
    candidate == entry || candidate.starts_with(&format!("{entry}/"))
}

/// True when a `$HOME`-relative dir hosts long-lived secrets and must never
/// be auto-approved for a persistent writable bind.
fn is_secret_dir(dir: &str) -> bool {
    SECRET_DIRS
        .iter()
        .any(|s| dir == *s || dir.starts_with(&format!("{s}/")))
}

/// Upper bound on denials returned by [`summarize_denials_from_log`]: one
/// advisory pass over a bounded summary, not a second copy of the log.
pub const MAX_SUMMARIZED_DENIALS: usize = 64;

/// Recover sandbox denials from a job's captured stderr/log text: lines
/// mentioning a read-only filesystem (`EROFS`, `Read-only file system`) map
/// to [`DenialKind::ReadOnlyFs`], missing-file lines (`ENOENT`, `No such
/// file or directory`) to [`DenialKind::NotFound`]. The denied path is the
/// first absolute-path token on the line; the binary is unknown from logs
/// alone and recorded as `unknown` for the caller to attribute. Repeats
/// aggregate into `count`. At most [`MAX_SUMMARIZED_DENIALS`] distinct
/// denials are returned; anything else is ignored.
#[must_use]
pub fn summarize_denials_from_log(log: &str) -> Vec<Denial> {
    let mut counts: HashMap<(String, DenialKind), u32> = HashMap::new();
    for line in log.lines() {
        let kind = if line.contains("EROFS") || line.contains("Read-only file system") {
            DenialKind::ReadOnlyFs
        } else if line.contains("ENOENT") || line.contains("No such file or directory") {
            DenialKind::NotFound
        } else {
            continue;
        };
        let Some(path) = first_absolute_path(line) else {
            continue;
        };
        let count = counts.entry((path, kind)).or_insert(0);
        *count = count.saturating_add(1);
        if counts.len() >= MAX_SUMMARIZED_DENIALS {
            break;
        }
    }
    counts
        .into_iter()
        .map(|((path, kind), count)| Denial {
            binary: "unknown".to_string(),
            path,
            kind,
            count,
        })
        .collect()
}

/// Render proposals as human-readable advice lines (what a future
/// `sandbox advise` CLI prints): rule, dir, confidence, containment against
/// `boundary`, and whether a human must approve. Pure and bounded: one line
/// per proposal plus a header.
#[must_use]
pub fn render_advice(proposals: &[Proposal], boundary: &[String]) -> String {
    if proposals.is_empty() {
        return "No sandbox mount proposals: no actionable denials observed.".to_string();
    }
    let mut out = String::from("Sandbox mount proposals (highest confidence first):\n");
    for proposal in proposals {
        let containment = match check_within_boundary(&proposal.suggested_dir, boundary) {
            Containment::Within => "already-covered",
            Containment::Exceeds => "exceeds-boundary",
        };
        let approval = if proposal.needs_explicit_approval {
            "needs-human-approval"
        } else {
            "auto-appliable"
        };
        out.push_str(&format!(
            "- {}: add `{}` to writable_home_dirs (confidence {:.2}, {containment}, {approval})\n  {}\n",
            proposal.rule_name,
            proposal.suggested_dir,
            proposal.confidence,
            proposal.rationale,
        ));
    }
    out
}

fn first_absolute_path(line: &str) -> Option<String> {
    line.split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '`' | '(' | ')' | '[' | ']'))
        .filter(|t| t.starts_with('/') && t.len() > 1)
        .find_map(|t| {
            let trimmed = t.trim_matches(|c| matches!(c, ',' | '.' | ';' | ':'));
            (trimmed.len() > 1).then(|| trimmed.to_string())
        })
}

fn generate_rule_name(dir: &str) -> String {
    let sanitized: String = dir
        .replace(['.', '/', '-'], "_")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    format!("mount_{sanitized}")
}

fn compute_confidence(count: u32, well_known: bool) -> f32 {
    let mut score: f32 = 0.5;
    if count >= 10 {
        score += 0.2;
    } else if count >= 3 {
        score += 0.1;
    }
    if well_known {
        score += 0.15;
    }
    score.clamp(0.1, 0.95)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_top_level_dotdir() {
        assert_eq!(
            suggest_writable_dir("/home/u/.cursor/projects/abc", "/home/u"),
            Some(".cursor".to_string())
        );
    }

    #[test]
    fn keeps_state_prefix_depth() {
        assert_eq!(
            suggest_writable_dir("/home/u/.local/share/opencode/log/x.log", "/home/u"),
            Some(".local/share/opencode".to_string())
        );
        assert_eq!(
            suggest_writable_dir("/home/u/.config/cursor/chats/id", "/home/u"),
            Some(".config/cursor".to_string())
        );
    }

    #[test]
    fn outside_home_has_no_suggestion() {
        assert_eq!(suggest_writable_dir("/tmp/x", "/home/u"), None);
        assert_eq!(suggest_writable_dir("/home/other/.cursor", "/home/u"), None);
    }

    #[test]
    fn groups_denials_and_sorts_by_confidence() {
        let denials = vec![
            Denial {
                binary: "cursor-agent".to_string(),
                path: "/home/u/.cursor/projects/a".to_string(),
                kind: DenialKind::ReadOnlyFs,
                count: 12,
            },
            Denial {
                binary: "cursor-agent".to_string(),
                path: "/home/u/.cursor/projects/b".to_string(),
                kind: DenialKind::ReadOnlyFs,
                count: 1,
            },
            Denial {
                binary: "sh".to_string(),
                path: "/home/u/.novel/state".to_string(),
                kind: DenialKind::ReadOnlyFs,
                count: 1,
            },
            Denial {
                binary: "sh".to_string(),
                path: "/tmp/x".to_string(),
                kind: DenialKind::ReadOnlyFs,
                count: 50,
            },
        ];
        let proposals = generate_proposals(&denials, "/home/u");
        assert_eq!(proposals.len(), 2);
        assert_eq!(proposals[0].suggested_dir, ".cursor");
        assert!(proposals[0].confidence >= proposals[1].confidence);
    }

    #[test]
    fn secret_dir_needs_explicit_approval() {
        let denials = vec![Denial {
            binary: "git".to_string(),
            path: "/home/u/.ssh/config".to_string(),
            kind: DenialKind::ReadOnlyFs,
            count: 1,
        }];
        let proposals = generate_proposals(&denials, "/home/u");
        assert_eq!(proposals.len(), 1);
        assert!(proposals[0].needs_explicit_approval);

        let denials = vec![Denial {
            binary: "cursor-agent".to_string(),
            path: "/home/u/.cursor/projects/a".to_string(),
            kind: DenialKind::ReadOnlyFs,
            count: 1,
        }];
        let proposals = generate_proposals(&denials, "/home/u");
        assert!(!proposals[0].needs_explicit_approval);
    }

    #[test]
    fn log_summary_parses_erofs_and_enoent_lines() {
        let log = "touch: cannot touch '/home/u/.cursor/x': Read-only file system\n\
                   cat: /home/u/.novel/f: No such file or directory\n\
                   ok line without denials\n\
                   thread panicked without a path: EROFS\n\
                   touch: cannot touch '/home/u/.cursor/x': Read-only file system\n";
        let mut denials = summarize_denials_from_log(log);
        denials.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(denials.len(), 2);
        let cursor = denials
            .iter()
            .find(|d| d.path == "/home/u/.cursor/x")
            .unwrap();
        assert_eq!(cursor.kind, DenialKind::ReadOnlyFs);
        assert_eq!(cursor.count, 2);
        let novel = denials
            .iter()
            .find(|d| d.path == "/home/u/.novel/f")
            .unwrap();
        assert_eq!(novel.kind, DenialKind::NotFound);
        assert_eq!(novel.count, 1);
    }

    #[test]
    fn render_advice_names_containment_and_approval() {
        let denials = vec![Denial {
            binary: "cursor-agent".to_string(),
            path: "/home/u/.cursor/projects/a".to_string(),
            kind: DenialKind::ReadOnlyFs,
            count: 5,
        }];
        let proposals = generate_proposals(&denials, "/home/u");
        let boundary = vec![".agentflare".to_string()];
        let text = render_advice(&proposals, &boundary);
        assert!(text.contains("mount__cursor"));
        assert!(text.contains("exceeds-boundary"));
        assert!(text.contains("auto-appliable"));

        let covered = render_advice(&proposals, &[".cursor".to_string()]);
        assert!(covered.contains("already-covered"));

        assert!(render_advice(&[], &boundary).contains("No sandbox mount proposals"));
    }

    #[test]
    fn containment_covers_exact_and_children_only() {
        let boundary = vec![".cursor".to_string(), ".config/cursor".to_string()];
        assert_eq!(
            check_within_boundary(".cursor", &boundary),
            Containment::Within
        );
        assert_eq!(
            check_within_boundary(".cursor/projects", &boundary),
            Containment::Within
        );
        assert_eq!(
            check_within_boundary(".cursor2", &boundary),
            Containment::Exceeds
        );
        assert_eq!(
            check_within_boundary(".ssh", &boundary),
            Containment::Exceeds
        );
        assert_eq!(
            check_within_boundary("../x", &boundary),
            Containment::Exceeds
        );
    }
}
