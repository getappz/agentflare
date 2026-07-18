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
