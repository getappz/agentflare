use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::LazyLock;

/// Per-project skill-advisory settings, stored as `.agentflare/settings.json`
/// at the repo root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillAdvisorySettings {
    #[serde(default = "default_proactivity")]
    pub proactivity_level: String, // "off" | "quiet" | "active"
    #[serde(default)]
    pub skill_overrides: std::collections::HashMap<String, SkillOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillOverride {
    #[serde(default)]
    pub snooze_until: i64, // epoch seconds; 0 = not snoozed
    #[serde(default)]
    pub dismissed: bool,
}

fn default_proactivity() -> String {
    "off".to_string()
}

impl Default for SkillAdvisorySettings {
    fn default() -> Self {
        Self {
            proactivity_level: "off".to_string(),
            skill_overrides: std::collections::HashMap::new(),
        }
    }
}

static SETTINGS_PATH: LazyLock<PathBuf> = LazyLock::new(|| {
    crate::mcp_server::AgentflareMcp::repo_root()
        .join(".agentflare")
        .join("settings.json")
});

pub fn load_settings() -> SkillAdvisorySettings {
    std::fs::read_to_string(&*SETTINGS_PATH)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

// TODO(task-6-followup): no CLI/MCP path calls this yet -- the snooze/dismiss
// write side (e.g. `skill snooze <name>`) is unbuilt, only the read side
// (proactive_suggestions() checking skill_overrides) exists. Remove this
// allow once a caller lands.
#[allow(dead_code)]
pub fn save_settings(settings: &SkillAdvisorySettings) {
    if let Some(parent) = SETTINGS_PATH.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(settings) {
        let _ = std::fs::write(&*SETTINGS_PATH, bytes);
    }
}

/// Generate proactive skill suggestions: runs `session_context_queries()` →
/// `classify()` → `find_skills()`, filters by settings, returns formatted
/// text suitable for hook injection. Returns None when no suggestions pass
/// filters or proactivity level is "off".
pub fn proactive_suggestions() -> Option<String> {
    let settings = load_settings();
    if settings.proactivity_level == "off" {
        return None;
    }

    let queries = crate::skill_detect::session_context_queries();
    if queries.is_empty() {
        return None;
    }

    let db_path = crate::paths::skills_db_path();
    let mut registry = skill_registry::Registry::open_default(&db_path).ok()?;
    registry.ensure_fresh(crate::components::detected_skill_agents).ok()?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut suggestions: Vec<String> = Vec::new();
    let threshold = if settings.proactivity_level == "quiet" {
        0.8
    } else {
        0.5
    };

    for q in &queries {
        let intent = crate::skill_detect::classify(q);
        if intent.confidence < threshold {
            continue;
        }
        let skills = crate::skill_detect::find_skills(
            &intent,
            &registry,
            3,
            |_| None::<Vec<f32>>,
            |_| None::<Vec<f32>>,
        );
        let Ok(skills) = skills else { continue };
        for s in &skills {
            let name = &s.name;
            let override_ = settings.skill_overrides.get(name);
            if let Some(o) = override_
                && (o.dismissed || (o.snooze_until > 0 && o.snooze_until > now))
            {
                continue;
            }
            suggestions.push(format!(
                "  - {}: {} (confidence {:.0}%)",
                name,
                s.description,
                s.score * 100.0
            ));
        }
    }

    if suggestions.is_empty() {
        return None;
    }

    suggestions.sort();
    suggestions.dedup();
    Some(format!(
        "\nRelevant skills for this project:\n{}",
        suggestions.join("\n")
    ))
}
