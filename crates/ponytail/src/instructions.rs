use crate::config;
use std::path::Path;

static EMBEDDED_SKILL: &str = include_str!("skill.md");

pub struct Instructions {
    #[allow(dead_code)]
    pub mode: String,
    pub body: String,
}

const SKILL_URL: &str =
    "https://raw.githubusercontent.com/DietrichGebert/ponytail/main/skills/ponytail/SKILL.md";

fn find_workspace_agents_md() -> Option<String> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let path = dir.join("AGENTS.md");
        if let Ok(content) = std::fs::read_to_string(&path) {
            if content.contains("PONYTAIL") {
                return Some(content);
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

pub fn skill_cache_path() -> std::path::PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("agentflare")
        .join("ponytail")
        .join("SKILL.md")
}

pub fn download_skill() -> Result<String, String> {
    let resp = ureq::get(SKILL_URL)
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .map_err(|e| format!("fetch failed: {e}"))?;
    if resp.status() != 200 {
        return Err(format!("HTTP {}", resp.status()));
    }
    let body = resp.into_string().map_err(|e| format!("read failed: {e}"))?;
    let path = skill_cache_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    std::fs::write(&path, &body).map_err(|e| format!("write: {e}"))?;
    Ok(path.display().to_string())
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
        return Instructions { mode: effective, body };
    }

    let skill_body = if let Some(path) = skill_path {
        std::fs::read_to_string(path).unwrap_or_else(|_| EMBEDDED_SKILL.to_string())
    } else {
        std::fs::read_to_string(skill_cache_path())
            .ok()
            .or_else(find_workspace_agents_md)
            .unwrap_or_else(|| EMBEDDED_SKILL.to_string())
    };

    let filtered = filter_skill_body(&skill_body, &effective);

    Instructions {
        mode: effective,
        body: filtered,
    }
}

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

#[allow(dead_code)]
pub fn fallback_instructions(mode: &str) -> String {
    let m = config::normalize_mode(mode).unwrap_or(config::DEFAULT_MODE);
    format!(
        "PONYTAIL MODE ACTIVE — level: {m}\n\n\
         You are a lazy senior developer. Lazy means efficient, not careless — \
         less work for the same result. The best code is the code never written.\n\n\
         ## The ladder\n\n\
         1. Does this need to exist at all? (YAGNI)\n\
         2. Already in this codebase? Reuse it.\n\
         3. Stdlib does it? Use it.\n\
         4. Native platform feature covers it? Use it.\n\
         5. Already-installed dependency solves it? Use it.\n\
         6. Can it be one line? One line.\n\
         7. Only then: the minimum code that works.\n\n\
         ## Rules\n\n\
         No unrequested abstractions. No boilerplate. Deletion over addition.\n\
         Code first, then at most three lines: what was skipped, when to add it.\n\
         Never simplify away: input validation, error handling, security, accessibility.\n\n\
         NEVER invent APIs, functions, or variables that don't exist in the codebase.\n\
         Always verify the API surface before using it — read the file or docs first.\n\
         Prefer searching the codebase over assuming. Trust but verify.\n\n\
         ## Persona boundary\n\n\
         Act the role, never label it. Don't mention ponytail mode, intensity\n\
         levels, or persona names in replies. The user knows what they asked for.\n\n\
         ## Simplification markers\n\n\
         Mark deliberate shortcuts with a `ponytail:` comment. One line only:\n\
         `ponytail: <what was skipped>, add when <condition>`\n\
         If the explanation is longer than the code, delete the explanation.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_generates_for_mode() {
        let f = fallback_instructions("full");
        assert!(f.contains("PONYTAIL MODE ACTIVE"));
        assert!(f.contains("The ladder"));
        assert!(f.contains("Persona boundary"));
        assert!(f.contains("Simplification markers"));
    }

    #[test]
    fn build_uses_embedded_skill() {
        let ins = build("full", None);
        assert!(!ins.body.is_empty());
        assert_eq!(ins.mode, "full");
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
