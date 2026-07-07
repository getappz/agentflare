use crate::ponytail::config;

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

    let cmd = prompt
        .strip_prefix("/ponytail")
        .or_else(|| prompt.strip_prefix("@ponytail"))
        .or_else(|| prompt.strip_prefix("$ponytail"))?;

    let parts: Vec<&str> = cmd.split_whitespace().collect();
    let sub = parts.first().copied().unwrap_or("");
    let arg = parts.get(1).copied().unwrap_or("");

    if sub.is_empty() || sub == "lite" || sub == "full" || sub == "ultra" {
        let mode = if sub.is_empty() { "full" } else { sub };
        config::normalize_config_mode(mode)?;
        return Some(SwitchAction::SetMode(mode.to_string()));
    }

    match sub {
        "off" => Some(SwitchAction::Off),
        "default" => {
            let dmode = arg;
            if dmode.is_empty() {
                return None;
            }
            config::normalize_config_mode(dmode)?;
            Some(SwitchAction::SetDefault(dmode.to_string()))
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
}
