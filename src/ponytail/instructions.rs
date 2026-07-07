use crate::ponytail::config;
use std::path::Path;

static EMBEDDED_SKILL: &str = include_str!("skill.md");

pub struct Instructions {
    #[allow(dead_code)]
    pub mode: String,
    pub body: String,
}

const SKILL_URL: &str =
    "https://raw.githubusercontent.com/DietrichGebert/ponytail/main/skills/ponytail/SKILL.md";

pub fn skill_cache_path() -> std::path::PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("agentflare")
        .join("ponytail")
        .join("SKILL.md")
}

pub fn download_skill() -> Result<String, String> {
    let resp = ureq::get(SKILL_URL)
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
    let effective = config::normalize_persisted_mode(mode)
        .unwrap_or(config::DEFAULT_MODE);

    let skill_body = if let Some(path) = skill_path {
        std::fs::read_to_string(path).unwrap_or_else(|_| EMBEDDED_SKILL.to_string())
    } else {
        let cache = dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("agentflare")
            .join("ponytail")
            .join("SKILL.md");
        std::fs::read_to_string(&cache).unwrap_or_else(|_| EMBEDDED_SKILL.to_string())
    };

    let filtered = filter_skill_body(&skill_body, effective);

    Instructions {
        mode: effective.to_string(),
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
         You are a lazy senior developer. Lazy means efficient, not careless.\n\n\
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
         Never simplify away: input validation, error handling, security, accessibility."
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
