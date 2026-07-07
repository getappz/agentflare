use crate::config;
use crate::sub_skills;

pub enum SwitchAction {
    SetMode(String),
    SetSession(String),
    SetDefault(String),
    Off,
    Report,
}

fn all_skill_names() -> Vec<String> {
    let mut names: Vec<String> = ["review", "audit", "debt", "gain", "help", "playbook"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    names.extend(sub_skills::custom_skill_names());
    names
}

fn all_mode_names() -> Vec<String> {
    let mut names: Vec<String> = ["lite", "full", "ultra"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    names.extend(all_skill_names());
    names
}

pub fn detect(input: &str) -> Option<SwitchAction> {
    let prompt = input.trim().to_lowercase();

    if config::is_deactivation(&prompt) {
        return Some(SwitchAction::Off);
    }

    for name in all_mode_names() {
        let prefixed = format!("/ponytail-{name}");
        let alt = format!("/ponytail:{name}");
        if prompt == prefixed || prompt.starts_with(&format!("{prefixed} ")) {
            let normalized = config::normalize_config_mode(&name)?;
            return Some(SwitchAction::SetMode(normalized.to_string()));
        }
        if prompt == alt || prompt.starts_with(&format!("{alt} ")) {
            let normalized = config::normalize_config_mode(&name)?;
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

    if sub.is_empty() {
        return Some(SwitchAction::Report);
    }

    if sub == "lite" || sub == "full" || sub == "ultra" {
        let normalized = config::normalize_config_mode(sub)?;
        return Some(SwitchAction::SetMode(normalized.to_string()));
    }

    match sub {
        "off" => Some(SwitchAction::Off),
        "status" => Some(SwitchAction::Report),
        "session" => {
            let smode = arg;
            if smode.is_empty() {
                return None;
            }
            let normalized = config::normalize_config_mode(smode)?;
            Some(SwitchAction::SetSession(normalized.to_string()))
        }
        s if all_skill_names().iter().any(|n| n == s) => {
            let normalized = config::normalize_config_mode(s)?;
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

    #[test]
    fn detects_sub_skill_playbook() {
        assert!(matches!(detect("/ponytail-playbook"), Some(SwitchAction::SetMode(m)) if m == "playbook"));
    }

    #[test]
    fn detects_session_mode() {
        assert!(matches!(detect("/ponytail session ultra"), Some(SwitchAction::SetSession(m)) if m == "ultra"));
        assert!(detect("/ponytail session").is_none());
    }

    #[test]
    fn detects_status() {
        assert!(matches!(detect("/ponytail status"), Some(SwitchAction::Report)));
    }

    #[test]
    fn detects_bare_as_report() {
        assert!(matches!(detect("/ponytail"), Some(SwitchAction::Report)));
    }

    #[test]
    fn detects_mode_shortcut() {
        assert!(matches!(detect("/ponytail-ultra"), Some(SwitchAction::SetMode(m)) if m == "ultra"));
        assert!(matches!(detect("/ponytail-lite"), Some(SwitchAction::SetMode(m)) if m == "lite"));
    }
}
