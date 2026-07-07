use crate::config;

pub enum SwitchAction {
    SetMode(String),
    SetDefault(String),
    Off,
}

pub fn detect(input: &str) -> Option<SwitchAction> {
    let prompt = input.trim().to_lowercase();

    if config::is_deactivation(&prompt) {
        return Some(SwitchAction::Off);
    }

    for skill in &["review", "audit", "debt", "gain", "help"] {
        let prefixed = format!("/ponytail-{skill}");
        let alt = format!("/ponytail:{skill}");
        if prompt == prefixed || prompt.starts_with(&format!("{prefixed} ")) {
            let normalized = config::normalize_config_mode(skill)?;
            return Some(SwitchAction::SetMode(normalized.to_string()));
        }
        if prompt == alt || prompt.starts_with(&format!("{alt} ")) {
            let normalized = config::normalize_config_mode(skill)?;
            return Some(SwitchAction::SetMode(normalized.to_string()));
        }
    }

    let cmd = prompt
        .strip_prefix("/ponytail")
        .or_else(|| prompt.strip_prefix("@ponytail"))
        .or_else(|| prompt.strip_prefix("$ponytail"))?;

    let parts: Vec<&str> = cmd.split_whitespace().collect();
    let sub = parts.first().copied().unwrap_or("");
    let arg = parts.get(1).copied().unwrap_or("");

    if sub.is_empty() || sub == "lite" || sub == "full" || sub == "ultra" {
        let mode = if sub.is_empty() { "full" } else { sub };
        let normalized = config::normalize_config_mode(mode)?;
        return Some(SwitchAction::SetMode(normalized.to_string()));
    }

    match sub {
        "off" => Some(SwitchAction::Off),
        "review" | "audit" | "debt" | "gain" | "help" => {
            let normalized = config::normalize_config_mode(sub)?;
            Some(SwitchAction::SetMode(normalized.to_string()))
        }
        "default" => {
            let dmode = arg;
            if dmode.is_empty() {
                return None;
            }
            let normalized = config::normalize_config_mode(dmode)?;
            Some(SwitchAction::SetDefault(normalized.to_string()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_mode_switch() {
        assert!(matches!(detect("/ponytail lite"), Some(SwitchAction::SetMode(m)) if m == "lite"));
        assert!(matches!(detect("/ponytail full"), Some(SwitchAction::SetMode(m)) if m == "full"));
    }

    #[test]
    fn detects_off() {
        assert!(matches!(detect("/ponytail off"), Some(SwitchAction::Off)));
    }

    #[test]
    fn detects_deactivation_phrase() {
        assert!(matches!(detect("stop ponytail"), Some(SwitchAction::Off)));
    }

    #[test]
    fn detects_default() {
        assert!(matches!(detect("/ponytail default ultra"), Some(SwitchAction::SetDefault(m)) if m == "ultra"));
    }

    #[test]
    fn ignores_false_positives() {
        assert!(detect("let's talk about ponytail").is_none());
        assert!(detect("").is_none());
    }

    #[test]
    fn detects_sub_skill_review() {
        assert!(matches!(detect("/ponytail-review"), Some(SwitchAction::SetMode(m)) if m == "review"));
    }

    #[test]
    fn detects_sub_skill_audit() {
        assert!(matches!(detect("/ponytail-audit"), Some(SwitchAction::SetMode(m)) if m == "audit"));
    }

    #[test]
    fn detects_sub_skill_inline() {
        assert!(matches!(detect("/ponytail review"), Some(SwitchAction::SetMode(m)) if m == "review"));
    }
}
