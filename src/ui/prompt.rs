//! Interactive input. Each helper returns a safe default when non-interactive
//! (or on cancel / I/O error) so a headless run never blocks on a prompt that
//! can't be answered.

use super::interactive;

/// Yes/No confirmation. Returns `default` when non-interactive, on cancel, or on
/// an I/O error — the command stays predictable and never hangs. `cliclack`
/// renders its own Yes/No affordance, so `prompt` should be a bare question with
/// no trailing `[Y/n]`.
pub fn confirm(prompt: &str, default: bool) -> bool {
    if !interactive() {
        return default;
    }
    cliclack::confirm(prompt)
        .initial_value(default)
        .interact()
        .unwrap_or(false)
}

/// Single-choice menu over `(value, label)` pairs. Returns the chosen value, or
/// `None` when non-interactive, when `items` is empty, or if the user cancels.
pub fn select(prompt: &str, items: &[(String, String)]) -> Option<String> {
    if !interactive() || items.is_empty() {
        return None;
    }
    let mut menu = cliclack::select(prompt);
    for (value, label) in items {
        menu = menu.item(value.clone(), label.as_str(), "");
    }
    menu.interact().ok()
}

/// Checklist over `(value, label, hint)` rows; `preselected` values start
/// checked and typing filters the rows. Returns the chosen values (possibly
/// none), or `None` when non-interactive, when `items` is empty, or if the
/// user cancels — so a caller can tell "picked nothing" from "backed out".
pub fn multiselect(
    prompt: &str,
    items: &[(String, String, String)],
    preselected: &[String],
) -> Option<Vec<String>> {
    if !interactive() || items.is_empty() {
        return None;
    }
    let mut menu = cliclack::multiselect(prompt)
        .initial_values(preselected.to_vec())
        .required(false)
        .filter_mode()
        .max_rows(15);
    for (value, label, hint) in items {
        menu = menu.item(value.clone(), label.as_str(), hint.as_str());
    }
    menu.interact().ok()
}

/// Masked password / secret input. Returns `None` when non-interactive, on
/// cancel, or on I/O error — the caller falls back to an env var or aborts.
/// `cliclack` renders a masked field, so this replaces raw `rpassword` calls
/// and stays consistent with the rest of the UI.
pub fn password(prompt: &str) -> Option<String> {
    if !interactive() {
        return None;
    }
    cliclack::password(prompt)
        .interact()
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test runs have no terminal, which is exactly the headless path.
    #[test]
    fn multiselect_is_none_off_a_terminal() {
        let items = [("a".to_string(), "A".to_string(), String::new())];
        assert_eq!(multiselect("pick", &items, &["a".to_string()]), None);
        assert_eq!(multiselect("pick", &[], &[]), None);
    }
}
