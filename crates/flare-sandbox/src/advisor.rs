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
    /// Permission denied on an existing path.
    Permission,
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
/// (prefer an ephemeral overlay, as agent state mounts already do).
const SECRET_DIRS: &[&str] = &[".ssh", ".aws", ".gnupg", ".pki"];

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
            let security_note = SECRET_DIRS
                .iter()
                .any(|s| dir == *s || dir.starts_with(&format!("{s}/")))
                .then(|| {
                    "Hosts long-lived secrets; prefer an ephemeral overlay over a persistent writable bind.".to_string()
                });
            Proposal {
                rule_name,
                suggested_dir: dir,
                rationale,
                confidence,
                security_note,
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
    fn secret_dir_gets_security_note() {
        let denials = vec![Denial {
            binary: "git".to_string(),
            path: "/home/u/.ssh/config".to_string(),
            kind: DenialKind::ReadOnlyFs,
            count: 1,
        }];
        let proposals = generate_proposals(&denials, "/home/u");
        assert_eq!(proposals.len(), 1);
        assert!(proposals[0].security_note.is_some());
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
