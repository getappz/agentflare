use crate::config;
use std::path::Path;

static EMBEDDED_SKILL: &str = include_str!("skill.md");

pub struct Instructions {
    #[allow(dead_code)]
    pub mode: String,
    pub body: String,
}

fn find_workspace_agents_md() -> Option<String> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let path = dir.join("AGENTS.md");
        if let Ok(content) = std::fs::read_to_string(&path)
            && content.contains("PONYTAIL")
        {
            return Some(content);
        }
        if !dir.pop() {
            return None;
        }
    }
}

pub fn build(mode: &str, skill_path: Option<&Path>) -> Instructions {
    // Custom skill names aren't in the static mode lists, so keep them as-is
    // instead of collapsing them to the default mode.
    let effective: String = config::normalize_persisted_mode(mode)
        .map(str::to_string)
        .or_else(|| crate::sub_skills::get_custom(mode).map(|_| mode.to_string()))
        .unwrap_or_else(|| config::DEFAULT_MODE.to_string());

    if crate::sub_skills::get(&effective).is_some() {
        return Instructions {
            mode: effective.clone(),
            body: format!(
                "PONYTAIL MODE ACTIVE — level: {effective}. Behavior defined by /ponytail-{effective} skill."
            ),
        };
    }

    // Custom skills have no harness-installed /ponytail-<name> skill to point
    // at, so their authored body is delivered inline.
    if let Some(body) = crate::sub_skills::get_custom(&effective) {
        return Instructions {
            mode: effective,
            body,
        };
    }

    let skill_body = if let Some(path) = skill_path {
        std::fs::read_to_string(path).unwrap_or_else(|_| EMBEDDED_SKILL.to_string())
    } else {
        find_workspace_agents_md().unwrap_or_else(|| EMBEDDED_SKILL.to_string())
    };

    let mut filtered = filter_skill_body(&skill_body, &effective);
    filtered.push_str(&compression_deconfliction());

    Instructions {
        mode: effective,
        body: filtered,
    }
}

/// If a known compression/persona plugin (e.g. caveman) is also wired into
/// the agent's settings, add a short note so the two don't read as
/// contradictory: ponytail governs code structure, the peer plugin governs
/// output style.
fn compression_deconfliction() -> String {
    let peers = config::detect_compression_plugins();
    if peers.is_empty() {
        return String::new();
    }
    format!(
        "\n\n## Compression plugin coexistence\n\n\
         Detected: {}. Ponytail governs WHAT to build (the ladder, YAGNI, \
         stdlib-first, minimal diffs). Defer to the other plugin for output \
         STYLE (brevity, tone, formatting). If rules conflict, the \
         structural rule (ponytail) wins for code decisions; the style rule \
         (peer plugin) wins for prose and formatting.",
        peers.join(", ")
    )
}

#[must_use]
pub fn filter_skill_body(body: &str, mode: &str) -> String {
    let effective = config::normalize_mode(mode).unwrap_or(config::DEFAULT_MODE);
    body.lines()
        .filter(|line| {
            if let Some(cap) = line.trim().strip_prefix("| **")
                && let Some(end) = cap.find("** |")
            {
                let label_mode = config::normalize_mode(&cap[..end]);
                if let Some(lm) = label_mode {
                    return lm == effective;
                }
            }
            if let Some(rest) = line.trim().strip_prefix("- ")
                && let Some(colon) = rest.find(':')
            {
                let label_mode = config::normalize_mode(rest[..colon].trim());
                if let Some(lm) = label_mode {
                    return lm == effective;
                }
            }
            true
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_uses_embedded_skill() {
        let ins = build("full", None);
        assert!(!ins.body.is_empty());
        assert_eq!(ins.mode, "full");
    }

    #[test]
    #[allow(unsafe_code)]
    fn build_appends_deconfliction_when_compression_plugin_present() {
        let _guard = config::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join("ponytail_test_instructions_compression");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), r#"{"plugins": ["caveman"]}"#).unwrap();
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", &dir) };

        let ins = build("full", None);
        assert!(ins.body.contains("Compression plugin coexistence"));
        assert!(ins.body.contains("caveman"));

        unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn embedded_skill_includes_anti_hallucination_guardrails() {
        assert!(EMBEDDED_SKILL.contains("Verify before you write"));
        assert!(EMBEDDED_SKILL.contains("bug with extra confidence"));
    }

    #[test]
    fn embedded_skill_includes_commit_and_comment_rules() {
        assert!(EMBEDDED_SKILL.contains("Co-Authored-By"));
        assert!(EMBEDDED_SKILL.contains("Conventional Commits"));
    }

    #[test]
    fn filter_skill_body_keeps_core_persona_content_in_every_mode() {
        // filter_skill_body only strips per-mode-labeled table/bullet rows —
        // plain-prose sections like these must survive filtering regardless
        // of active mode, since they're core persona, not an add-on.
        for mode in ["lite", "full", "ultra"] {
            let filtered = filter_skill_body(EMBEDDED_SKILL, mode);
            assert!(filtered.contains("Verify before you write"), "mode={mode}");
            assert!(filtered.contains("Co-Authored-By"), "mode={mode}");
        }
    }

    #[test]
    fn filter_keeps_non_mode_lines() {
        let input = "some rule\n| **lite** | lite only |\n| **full** | full only |\nother rule";
        let filtered = filter_skill_body(input, "full");
        assert!(filtered.contains("some rule"));
        assert!(filtered.contains("full only"));
        assert!(!filtered.contains("lite only"));
        assert!(filtered.contains("other rule"));
    }
}
