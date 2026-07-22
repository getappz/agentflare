//! Skill source adapters: flat SKILL.md directories and the Claude plugin cache.

use crate::frontmatter::parse_frontmatter;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct SkillEntry {
    pub name: String,
    pub source: String,
    pub path: PathBuf,
    pub description: String,
    pub body: String,
    pub neg_text: String,
    pub tags: String, // space-joined, FTS column
    pub est_tokens: i64,
    pub mtime: i64,
    pub bandit_alpha: f64,
    pub bandit_beta: f64,
    /// Compressed shadow copy of this skill, when one exists.
    pub shadow_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub enum SourceKind {
    /// `<root>/<skill-name>/SKILL.md`
    FlatDir(PathBuf),
    /// `<root>/<marketplace>/<plugin>/<version>/skills/<skill-name>/SKILL.md`,
    /// newest version per (plugin, skill).
    PluginCache(PathBuf),
}

#[derive(Debug, Clone)]
pub struct Source {
    pub id: String,
    pub kind: SourceKind,
}

#[derive(Debug, Default)]
pub struct ScanOutput {
    pub entries: Vec<SkillEntry>,
    pub skipped: usize,
}

pub fn est_tokens(bytes: u64) -> i64 {
    (bytes as f64 / 3.7) as i64
}

/// Numeric tuple from a version dir name ("6.1.1" -> [6,1,1]) for newest-wins.
fn version_key(dir_name: &str) -> Vec<u64> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    for c in dir_name.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            parts.push(cur.parse().unwrap_or(0));
            cur.clear();
        }
    }
    if !cur.is_empty() {
        parts.push(cur.parse().unwrap_or(0));
    }
    parts
}

static NEGATION_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(do not use|skip|not for|never use)\b").unwrap()
});

/// Split text at the first negation marker. Everything before stays in the
/// positive portion; everything from the marker onward goes to neg_text.
/// Returns (positive, neg_text).
fn split_negation(text: &str) -> (String, String) {
    match NEGATION_RE.find(text) {
        Some(m) => {
            let pos = text[..m.start()].trim().to_string();
            let neg = text[m.start()..].trim().to_string();
            (pos, neg)
        }
        None => (text.to_string(), String::new()),
    }
}

/// Directories/files to skip during source scanning.
static IGNORE_DIRS: &[&str] = &["__pycache__", "node_modules", ".git", ".venv"];

fn is_ignored(entry: &std::fs::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .is_some_and(|n| IGNORE_DIRS.contains(&n))
}

fn read_entry(id: &str, path: &Path) -> Option<SkillEntry> {
    let text = std::fs::read_to_string(path).ok()?;
    let (fm, body) = parse_frontmatter(&text)?;
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let dir_name = path.parent()?.file_name()?.to_string_lossy().to_string();
    let desc = fm.description.unwrap_or_default();
    let (description, desc_neg) = split_negation(&desc);
    let body_text = body.trim();
    let (body, body_neg) = split_negation(body_text);
    let neg_text = match (desc_neg.is_empty(), body_neg.is_empty()) {
        (true, true) => String::new(),
        (false, true) => desc_neg,
        (true, false) => body_neg,
        (false, false) => format!("{desc_neg} {body_neg}"),
    };
    Some(SkillEntry {
        name: fm.name.unwrap_or(dir_name),
        source: id.to_string(),
        path: path.to_path_buf(),
        description,
        body,
        neg_text,
        tags: fm.tags.join(" "),
        est_tokens: est_tokens(meta.len()),
        mtime,
        bandit_alpha: 1.0,
        bandit_beta: 1.0,
        shadow_path: None,
    })
}

static SHADOW_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"compressed-from:\s+(.+?)\s+\d+B").unwrap());

/// `<!-- compressed-from: <path> <N>B → <M>B, <date> -->` in the first 2000 chars.
fn shadow_origin(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path).ok()?;
    let head: String = text.chars().take(2000).collect();
    SHADOW_RE
        .captures(&head)
        .map(|c| PathBuf::from(c[1].to_string()))
}

pub fn scan_sources(sources: &[Source]) -> ScanOutput {
    let mut out = ScanOutput::default();
    for src in sources {
        match &src.kind {
            SourceKind::FlatDir(root) => {
                let Ok(read) = std::fs::read_dir(root) else {
                    continue;
                };
                for dir in read.flatten().filter(|e| !is_ignored(e)) {
                    let skill_md = dir.path().join("SKILL.md");
                    if !skill_md.is_file() {
                        continue;
                    }
                    match read_entry(&src.id, &skill_md) {
                        Some(e) => out.entries.push(e),
                        None => out.skipped += 1,
                    }
                }
            }
            SourceKind::PluginCache(root) => {
                // <root>/<marketplace>/<plugin>/<version>/skills/<skill>/SKILL.md
                let mut best: std::collections::HashMap<(String, String), PathBuf> =
                    std::collections::HashMap::new();
                for mkt in std::fs::read_dir(root).into_iter().flatten().flatten() {
                    for plugin in std::fs::read_dir(mkt.path())
                        .into_iter()
                        .flatten()
                        .flatten()
                        .filter(|e| !is_ignored(e))
                    {
                        for ver in std::fs::read_dir(plugin.path())
                            .into_iter()
                            .flatten()
                            .flatten()
                            .filter(|e| !is_ignored(e))
                        {
                            let skills = ver.path().join("skills");
                            for sk in std::fs::read_dir(&skills).into_iter().flatten().flatten() {
                                let f = sk.path().join("SKILL.md");
                                if !f.is_file() {
                                    continue;
                                }
                                let key = (
                                    plugin.file_name().to_string_lossy().to_string(),
                                    sk.file_name().to_string_lossy().to_string(),
                                );
                                let replace = match best.get(&key) {
                                    // parents: SKILL.md -> <skill> -> skills -> <version>
                                    Some(cur) => {
                                        let cur_ver = cur
                                            .ancestors()
                                            .nth(3)
                                            .and_then(|p| p.file_name())
                                            .map(|n| version_key(&n.to_string_lossy()))
                                            .unwrap_or_default();
                                        version_key(&ver.file_name().to_string_lossy()) > cur_ver
                                    }
                                    None => true,
                                };
                                if replace {
                                    best.insert(key, f);
                                }
                            }
                        }
                    }
                }
                for ((plugin, _skill), f) in best {
                    match read_entry(&format!("{}:{}", src.id, plugin), &f) {
                        Some(e) => out.entries.push(e),
                        None => out.skipped += 1,
                    }
                }
            }
        }
    }
    // Shadow pairing: user entries whose body carries the provenance marker
    // annotate their original and are dropped as standalone entries.
    let mut shadows: Vec<(usize, PathBuf)> = Vec::new(); // (entry idx of shadow, origin path)
    for (i, e) in out.entries.iter().enumerate() {
        if e.source.starts_with("claude-user")
            && let Some(origin) = shadow_origin(&e.path)
        {
            shadows.push((i, origin));
        }
    }
    let mut drop_idx: Vec<usize> = Vec::new();
    for (shadow_i, origin) in &shadows {
        let shadow_path = out.entries[*shadow_i].path.clone();
        let shadow_est_tokens = out.entries[*shadow_i].est_tokens;
        match out.entries.iter().position(|e| &e.path == origin) {
            Some(orig_i) => {
                out.entries[orig_i].shadow_path = Some(shadow_path);
                // skill_load serves the shadow body by default, so the
                // advertised cost must match what's actually served.
                out.entries[orig_i].est_tokens = shadow_est_tokens;
                drop_idx.push(*shadow_i);
            }
            None => {
                // orphan shadow: keep, flag as compressed via self-referential shadow_path
                out.entries[*shadow_i].shadow_path = Some(shadow_path);
            }
        }
    }
    drop_idx.sort_unstable_by(|a, b| b.cmp(a));
    for i in drop_idx {
        out.entries.remove(i);
    }
    out
}

/// Validate a skill entry has all required fields and its path is reachable.
/// Returns a list of validation warnings (non-fatal).
pub fn validate_entry(e: &SkillEntry) -> Vec<String> {
    let mut warnings = Vec::new();
    if e.name.is_empty() {
        warnings.push("name is empty".into());
    }
    if e.source.is_empty() {
        warnings.push("source is empty".into());
    }
    if e.description.is_empty() {
        warnings.push("description is empty".into());
    }
    if e.est_tokens <= 0 {
        warnings.push("est_tokens is zero or negative".into());
    }
    if !e.path.as_os_str().is_empty() && !e.path.exists() {
        warnings.push(format!("path does not exist: {}", e.path.display()));
    }
    warnings
}

/// Built-in source set. `detected_agents` are agent-registry ids (e.g. "codex");
/// non-Claude roots are contributed only for detected agents and only if the
/// directory exists.
pub fn default_sources(home: &Path, cwd: &Path, detected_agents: &[String]) -> Vec<Source> {
    let mut v = vec![
        Source {
            id: "claude-user".into(),
            kind: SourceKind::FlatDir(home.join(".claude").join("skills")),
        },
        Source {
            id: "claude-project".into(),
            kind: SourceKind::FlatDir(cwd.join(".claude").join("skills")),
        },
        Source {
            id: "claude-plugin".into(),
            kind: SourceKind::PluginCache(home.join(".claude").join("plugins").join("cache")),
        },
    ];
    // Conventional flat skill dirs for other agents; contributed only when the
    // agent is detected AND the dir exists (harmless no-op otherwise).
    let conventions: &[(&str, PathBuf)] = &[
        ("codex", home.join(".codex").join("skills")),
        ("cursor", home.join(".cursor").join("skills")),
        (
            "opencode",
            home.join(".config").join("opencode").join("skills"),
        ),
    ];
    for (agent, dir) in conventions {
        if detected_agents.iter().any(|a| a == agent) && dir.is_dir() {
            v.push(Source {
                id: (*agent).to_string(),
                kind: SourceKind::FlatDir(dir.clone()),
            });
        }
    }
    v.retain(|s| match &s.kind {
        SourceKind::FlatDir(p) | SourceKind::PluginCache(p) => p.is_dir(),
    });
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_skill(dir: &Path, name: &str, desc: &str, body: &str) {
        let d = dir.join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {desc}\n---\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn flat_dir_scan_produces_entries() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), "alpha", "Use when testing alpha things", "body");
        write_skill(tmp.path(), "beta", "Use when testing beta things", "body");
        let out = scan_sources(&[Source {
            id: "claude-user".into(),
            kind: SourceKind::FlatDir(tmp.path().to_path_buf()),
        }]);
        assert_eq!(out.entries.len(), 2);
        assert_eq!(out.skipped, 0);
        let alpha = out.entries.iter().find(|e| e.name == "alpha").unwrap();
        assert_eq!(alpha.source, "claude-user");
        assert!(alpha.est_tokens > 0);
    }

    #[test]
    fn malformed_frontmatter_is_skipped_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("broken");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("SKILL.md"), "no frontmatter at all").unwrap();
        write_skill(tmp.path(), "ok", "works", "body");
        let out = scan_sources(&[Source {
            id: "claude-user".into(),
            kind: SourceKind::FlatDir(tmp.path().to_path_buf()),
        }]);
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.skipped, 1);
    }

    #[test]
    fn plugin_cache_picks_newest_version() {
        let tmp = tempfile::tempdir().unwrap();
        for v in ["6.0.3", "6.1.1", "6.1.0"] {
            let d = tmp
                .path()
                .join("mkt")
                .join("superpowers")
                .join(v)
                .join("skills");
            write_skill(
                &d,
                "writing-skills",
                "Use when creating skills",
                &format!("v{v}"),
            );
        }
        let out = scan_sources(&[Source {
            id: "claude-plugin".into(),
            kind: SourceKind::PluginCache(tmp.path().to_path_buf()),
        }]);
        assert_eq!(out.entries.len(), 1);
        assert!(out.entries[0].path.to_string_lossy().contains("6.1.1"));
        // plugin-qualified name: "<plugin>:<skill>" is NOT used for `name`;
        // plugin identity rides in `source` as "claude-plugin:<plugin>".
        assert_eq!(out.entries[0].source, "claude-plugin:superpowers");
    }

    #[test]
    fn shadow_marker_pairs_with_original_and_user_copy_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let d = cache.join("mkt").join("cv").join("1.0.0").join("skills");
        write_skill(
            &d,
            "live",
            "Use when checking live sessions",
            "ORIGINAL BODY",
        );
        let orig = d.join("live").join("SKILL.md");

        let user = tmp.path().join("user-skills");
        let ud = user.join("live");
        fs::create_dir_all(&ud).unwrap();
        fs::write(
            ud.join("SKILL.md"),
            format!(
                "---\nname: live\ndescription: Use when checking live sessions\n---\n<!-- compressed-from: {} 12179B → 1725B, 2026-07-08 -->\nshort body",
                orig.display()
            ),
        )
        .unwrap();

        let out = scan_sources(&[
            Source {
                id: "claude-user".into(),
                kind: SourceKind::FlatDir(user),
            },
            Source {
                id: "claude-plugin".into(),
                kind: SourceKind::PluginCache(cache),
            },
        ]);
        // one logical skill: the plugin original carrying its shadow
        assert_eq!(out.entries.len(), 1);
        let e = &out.entries[0];
        assert_eq!(e.source, "claude-plugin:cv");
        let shadow_md_path = ud.join("SKILL.md");
        assert_eq!(e.shadow_path.as_deref(), Some(shadow_md_path.as_path()));
        // skill_load serves the shadow body by default, so est_tokens must
        // reflect the shadow file's size, not the (bigger) original's.
        let shadow_bytes = fs::metadata(&shadow_md_path).unwrap().len();
        assert_eq!(e.est_tokens, est_tokens(shadow_bytes));
        assert_ne!(e.est_tokens, est_tokens(fs::metadata(&orig).unwrap().len()));
    }

    #[test]
    fn orphan_shadow_is_kept_as_its_own_compressed_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let ud = tmp.path().join("gone");
        fs::create_dir_all(&ud).unwrap();
        fs::write(
            ud.join("SKILL.md"),
            "---\nname: gone\ndescription: d\n---\n<!-- compressed-from: C:/no/such/plugin/SKILL.md 10B → 5B, 2026-07-08 -->\nbody",
        )
        .unwrap();
        let out = scan_sources(&[Source {
            id: "claude-user".into(),
            kind: SourceKind::FlatDir(tmp.path().to_path_buf()),
        }]);
        assert_eq!(out.entries.len(), 1);
        assert_eq!(
            out.entries[0].shadow_path.as_deref(),
            Some(ud.join("SKILL.md").as_path())
        );
    }

    #[test]
    fn scan_sources_skips_ignored_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        // Valid skill
        write_skill(tmp.path(), "real-skill", "A real skill", "body");
        // Ignored dirs
        for ignored in ["__pycache__", "node_modules", ".git", ".venv"] {
            let d = tmp.path().join(ignored);
            fs::create_dir_all(&d).unwrap();
            fs::write(
                d.join("SKILL.md"),
                "---\nname: ghost\ndescription: should not appear\n---\nbody",
            )
            .unwrap();
        }
        let out = scan_sources(&[Source {
            id: "claude-user".into(),
            kind: SourceKind::FlatDir(tmp.path().to_path_buf()),
        }]);
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.entries[0].name, "real-skill");
    }

    #[test]
    fn split_negation_extracts_negation_marker() {
        let (pos, neg) = split_negation("Use for Claude API. Do NOT use for OpenAI.");
        assert_eq!(pos, "Use for Claude API.");
        assert_eq!(neg, "Do NOT use for OpenAI.");
    }

    #[test]
    fn split_negation_no_marker_returns_empty_neg() {
        let (pos, neg) = split_negation("Use for general automation");
        assert_eq!(pos, "Use for general automation");
        assert!(neg.is_empty());
    }

    #[test]
    fn split_negation_multiple_markers_splits_at_first() {
        let (pos, neg) =
            split_negation("General automation. Skip for file parsing. Not for data extraction.");
        assert_eq!(pos, "General automation.");
        assert!(
            neg.starts_with("Skip"),
            "neg should start at first marker: {neg}"
        );
    }

    #[test]
    fn split_negation_is_case_insensitive() {
        let (pos, neg) = split_negation("Use for Claude. skip for OpenAI. NEVER USE for Gemini.");
        assert_eq!(pos, "Use for Claude.");
        assert!(neg.starts_with("skip"), "neg={neg}");
    }

    #[test]
    fn read_entry_separates_negation_from_description_and_body() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("test-skill");
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("SKILL.md"),
            "---\nname: test-skill\ndescription: Use for coding tasks. Not for debugging.\n---\nGeneral code assistance. Never use for SQL.\n",
        )
        .unwrap();
        let e = read_entry("claude-user", &d.join("SKILL.md")).unwrap();
        assert_eq!(e.description, "Use for coding tasks.");
        assert_eq!(e.body, "General code assistance.");
        assert!(
            e.neg_text.contains("Not for debugging"),
            "neg_text should contain description negation: {}",
            e.neg_text
        );
        assert!(
            e.neg_text.contains("Never use for SQL"),
            "neg_text should contain body negation: {}",
            e.neg_text
        );
    }

    #[test]
    fn default_sources_gates_non_claude_agents_on_detection_and_existence() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join(".claude").join("skills")).unwrap();
        std::fs::create_dir_all(home.join(".codex").join("skills")).unwrap();
        // codex detected AND dir exists -> included; cursor not detected -> excluded
        let srcs = default_sources(home, home, &["codex".into()]);
        assert!(srcs.iter().any(|s| s.id == "codex"));
        assert!(!srcs.iter().any(|s| s.id == "cursor"));
    }
}
