//! `~/.bashenv` — sourced by Claude Code's non-interactive Bash tool via the
//! `BASH_ENV` env var (set in `~/.claude/settings.json`), since a
//! non-interactive shell reads neither `.bashrc` nor `.bash_profile`. Two
//! independent, marker-delimited blocks live in the same file:
//!
//! - "agentflare shims": a bash-function dispatcher that routes bare
//!   tool-name calls (git, npm, cargo, ...) through `lean-ctx -c` while an
//!   agent CLI is active — the bash-function-level companion to the PATH
//!   shims `shim_install.rs` installs (functions shadow PATH resolution, so
//!   this still fires even before/without those shims on PATH).
//! - "agentflare guard": a DEBUG-trap that blocks `rm -rf /`-class commands
//!   and force-push in agentflare-tracked repos, independent of PATH/shell
//!   aliasing.
//!
//! Never a whole-file overwrite: create the file if absent, otherwise patch
//! each marker block in place and leave everything else in the file alone
//! (the same convention `cli/git.rs`'s hook installer uses for
//! `~/.agentflare/githooks/`).

use crate::jsonc::{read_json_object, write_json_pretty};
use crate::paths::{claude_settings_path, home};
use serde_json::json;
use std::fs;
use std::path::PathBuf;

fn bashenv_path() -> PathBuf {
    home().join(".bashenv")
}

const SHIMS_START: &str = "# >>> agentflare shims >>>";
const SHIMS_END: &str = "# <<< agentflare shims <<<";
const GUARD_START: &str = "# >>> agentflare guard >>>";
const GUARD_END: &str = "# <<< agentflare guard <<<";

fn shims_block() -> String {
    let tools = crate::shim_install::bashenv_tool_list().join(" ");
    format!(
        r#"# Sourced by non-interactive bash via BASH_ENV (Claude Code settings.json env).
# OS-level output compression, combining lean-ctx's own alias/function dispatch
# (item #146) with this project's mise-style .agentflare directory scoping.
# No PATH dir, no generated files: one dispatcher + a function per tool name,
# defined fresh every shell start. Keep the name list synced with lean-ctx's
# own catalog / rewrite_registry.rs. `function name {{ }}` (no parens) accepts
# hyphens (docker-compose, golangci-lint), unlike `name() {{ }}`.
# project gate (mise-shim style): only act inside agentflare-managed projects
# -- walk up from $PWD for a .agentflare marker, builtins only. Stop at
# $HOME: ~/.agentflare is the app's own data dir, not a project marker --
# without this the walk-up false-positives on anything under home. Shared by
# the shim dispatcher below and the af-guard DEBUG trap further down -- both
# need "is this actually an agentflare-tracked repo" before acting.
_af_in_scoped_project() {{
  local _af_pd=$PWD _af_pn
  while :; do
    [ "$_af_pd" = "$HOME" ] && return 1
    [ -e "$_af_pd/.agentflare" ] && return 0
    [ "$_af_pd" = / ] && return 1
    _af_pn=${{_af_pd%/*}}
    [ -z "$_af_pn" ] && _af_pn=/
    [ "$_af_pn" = "$_af_pd" ] && return 1
    _af_pd=$_af_pn
  done
}}
_af_dispatch() {{
  local _af_cmd=$1; shift
  if [ -n "${{LEAN_CTX_DISABLED:-}}" ] || [ -n "${{LEAN_CTX_NO_HOOK:-}}" ]; then command "$_af_cmd" "$@"; return; fi
  if [ -t 1 ]; then command "$_af_cmd" "$@"; return; fi
  if [ -z "${{CLAUDECODE:-}}" ] && [ -z "${{CURSOR_AGENT:-}}" ] && [ -z "${{CODEX_CLI_SESSION:-}}" ] \
     && [ -z "${{GEMINI_SESSION:-}}" ] && [ -z "${{CODEBUDDY:-}}" ] && [ -z "${{LEAN_CTX_AGENT:-}}" ]; then
    command "$_af_cmd" "$@"; return
  fi
  if ! _af_in_scoped_project; then command "$_af_cmd" "$@"; return; fi
  lean-ctx -c "$_af_cmd" "$@"
  local _af_rc=$?
  if [ "$_af_rc" -eq 126 ] || [ "$_af_rc" -eq 127 ]; then command "$_af_cmd" "$@"; return; fi
  return "$_af_rc"
}}
for _af_c in {tools}; do
  eval "function $_af_c {{ _af_dispatch $_af_c \"\$@\"; }}"
done
unset _af_c"#
    )
}

fn guard_block() -> &'static str {
    r#"# OS-level guardrails via bash DEBUG trap (prototype, item #217 adjacent).
# Sees every simple command bash is about to run (incl. absolute paths).
# Escape hatch: AF_GUARD_OFF=1. Rules are static v0; arai match_hook later.
if [ -n "${BASH_VERSION:-}" ] && [ -z "${AF_GUARD_OFF:-}" ]; then
  _af_guard() {
    case "$1" in
      # The harness wraps whole tool calls in `eval '...'` -- BASH_COMMAND then
      # holds the entire script text; skip the wrapper so patterns don't false-match
      # on strings/echoes. Every simple command INSIDE the eval still fires DEBUG.
      eval*|_af_guard*) return 0 ;;
      "rm -rf /"|"rm -rf /"[!a-zA-Z0-9]*|"rm -rf ~"*|"rm -rf \$HOME"*) return 1 ;;
      # NOTE: commit/merge/rebase-on-protected-branch is intentionally NOT
      # duplicated here -- .githooks/pre-commit already enforces it, fires
      # for every git client regardless of shell/PATH, and survives cases
      # (e.g. commands routed through lean-ctx's own exec wrapper) where
      # this bash-level DEBUG trap silently never fires at all.
      # Force-push is scoped to agentflare-tracked repos (mirrors
      # flare-git-core/classify.rs's in_scoped_project gate) -- broader than
      # .githooks/pre-push (which only blocks pushing the default branch);
      # this blocks force-push to ANY branch in a tracked repo.
      "git push"*" --force"*|"git push"*" -f "*|"git push"*" -f")
        _af_in_scoped_project || return 0
        return 1 ;;
    esac
    return 0
  }
  trap '_af_guard "$BASH_COMMAND" || { echo "[af-guard] BLOCKED: $BASH_COMMAND" >&2; exit 77; }' DEBUG
fi"#
}

/// Creates or patches a `start`/`end`-delimited block within `content`,
/// leaving everything outside the markers untouched. Returns the new content
/// and whether anything changed.
fn upsert_block(content: &str, start: &str, end: &str, block: &str) -> (String, bool) {
    let full_block = format!("{start}\n{block}\n{end}");
    if let Some(s) = content.find(start) {
        // A start marker with no matching end is a truncated/corrupted block
        // (e.g. a user's manual edit cut it off mid-way) -- leave it alone
        // rather than guessing where it ends and appending a second copy
        // after it.
        let Some(rel_e) = content[s..].find(end) else {
            return (content.to_string(), false);
        };
        let e = s + rel_e + end.len();
        if content[s..e] == full_block {
            return (content.to_string(), false);
        }
        return (
            format!("{}{full_block}{}", &content[..s], &content[e..]),
            true,
        );
    }
    let sep = if content.is_empty() || content.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    (format!("{content}{sep}{full_block}\n"), true)
}

fn has_current_block(content: &str, start: &str, end: &str, block: &str) -> bool {
    let full_block = format!("{start}\n{block}\n{end}");
    content.find(start).is_some_and(|s| {
        content[s..]
            .find(end)
            .is_some_and(|rel_e| content[s..s + rel_e + end.len()] == full_block)
    })
}

fn bash_env_value() -> String {
    bashenv_path().to_string_lossy().replace('\\', "/")
}

fn bash_env_is_set() -> bool {
    read_json_object(&claude_settings_path(), || json!({}))
        .get("env")
        .and_then(|e| e.get("BASH_ENV"))
        .and_then(|v| v.as_str())
        == Some(bash_env_value().as_str())
}

/// Outcome of wiring `BASH_ENV` into `~/.claude/settings.json`, distinct
/// from a plain bool so a write failure can't be reported as "already set".
enum EnvWireOutcome {
    AlreadySet,
    Wired,
    Failed,
}

fn set_bash_env_setting() -> EnvWireOutcome {
    if bash_env_is_set() {
        return EnvWireOutcome::AlreadySet;
    }
    let path = claude_settings_path();
    let mut settings = read_json_object(&path, || json!({}));
    let obj = settings.as_object_mut().unwrap();
    // `env` may already exist as something other than an object (null, a
    // stray string from a hand-edit, ...) -- coerce it rather than
    // unwrap-panicking on as_object_mut().
    if !obj.get("env").is_some_and(|v| v.is_object()) {
        obj.insert("env".to_string(), json!({}));
    }
    let env_obj = obj.get_mut("env").unwrap().as_object_mut().unwrap();
    env_obj.insert("BASH_ENV".to_string(), json!(bash_env_value()));
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if write_json_pretty(&path, &settings).is_ok() {
        EnvWireOutcome::Wired
    } else {
        EnvWireOutcome::Failed
    }
}

/// `true` once both marker blocks hold current content and `BASH_ENV` is
/// wired in `~/.claude/settings.json`.
pub fn is_installed() -> bool {
    let content = fs::read_to_string(bashenv_path()).unwrap_or_default();
    has_current_block(&content, SHIMS_START, SHIMS_END, &shims_block())
        && has_current_block(&content, GUARD_START, GUARD_END, guard_block())
        && bash_env_is_set()
}

/// Writes/patches both blocks in `~/.bashenv` (creating the file if absent)
/// and wires `BASH_ENV` into `~/.claude/settings.json`. Returns a status
/// message for `Component::apply`'s display.
pub fn ensure_installed() -> String {
    let path = bashenv_path();
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let (content, shims_changed) = upsert_block(&existing, SHIMS_START, SHIMS_END, &shims_block());
    let (content, guard_changed) = upsert_block(&content, GUARD_START, GUARD_END, guard_block());

    let file_changed = shims_changed || guard_changed;
    if file_changed {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(e) = fs::write(&path, &content) {
            return format!("cannot write {}: {e}", path.display());
        }
    }

    match (file_changed, set_bash_env_setting()) {
        (true, EnvWireOutcome::Wired) => format!(
            "{} written; BASH_ENV wired in ~/.claude/settings.json",
            path.display()
        ),
        (true, EnvWireOutcome::AlreadySet) => format!("{} written", path.display()),
        (true, EnvWireOutcome::Failed) => format!(
            "{} written; failed to wire BASH_ENV in ~/.claude/settings.json",
            path.display()
        ),
        (false, EnvWireOutcome::Wired) => "BASH_ENV wired in ~/.claude/settings.json".to_string(),
        (false, EnvWireOutcome::AlreadySet) => format!("{} already up to date", path.display()),
        (false, EnvWireOutcome::Failed) => {
            "failed to wire BASH_ENV in ~/.claude/settings.json".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn upsert_block_creates_when_absent() {
        let (content, changed) = upsert_block("", "# >>> x >>>", "# <<< x <<<", "body");
        assert!(changed);
        assert_eq!(content, "# >>> x >>>\nbody\n# <<< x <<<\n");
    }

    #[test]
    fn upsert_block_appends_after_unrelated_content() {
        let (content, changed) =
            upsert_block("# my own stuff\n", "# >>> x >>>", "# <<< x <<<", "body");
        assert!(changed);
        assert_eq!(content, "# my own stuff\n# >>> x >>>\nbody\n# <<< x <<<\n");
    }

    #[test]
    fn upsert_block_is_a_noop_when_already_current() {
        let first = upsert_block("", "# >>> x >>>", "# <<< x <<<", "body").0;
        let (content, changed) = upsert_block(&first, "# >>> x >>>", "# <<< x <<<", "body");
        assert!(!changed);
        assert_eq!(content, first);
    }

    #[test]
    fn upsert_block_replaces_stale_content_in_place_leaving_surroundings_alone() {
        let before = "# before\n# >>> x >>>\nold body\n# <<< x <<<\n# after\n";
        let (content, changed) = upsert_block(before, "# >>> x >>>", "# <<< x <<<", "new body");
        assert!(changed);
        assert_eq!(
            content,
            "# before\n# >>> x >>>\nnew body\n# <<< x <<<\n# after\n"
        );
    }

    #[test]
    fn upsert_block_leaves_a_truncated_marker_alone_instead_of_duplicating() {
        let corrupt = "# before\n# >>> x >>>\nhalf-written, no end marker\n";
        let (content, changed) = upsert_block(corrupt, "# >>> x >>>", "# <<< x <<<", "body");
        assert!(!changed);
        assert_eq!(content, corrupt);
    }

    #[test]
    fn has_current_block_false_when_markers_absent() {
        assert!(!has_current_block(
            "nothing here",
            "# >>> x >>>",
            "# <<< x <<<",
            "body"
        ));
    }

    #[test]
    fn has_current_block_false_when_stale() {
        let stale = "# >>> x >>>\nold\n# <<< x <<<\n";
        assert!(!has_current_block(
            stale,
            "# >>> x >>>",
            "# <<< x <<<",
            "new"
        ));
    }

    #[test]
    fn bashenv_tool_list_includes_git_and_uv_but_not_duplicated() {
        let tools = crate::shim_install::bashenv_tool_list();
        assert_eq!(tools.iter().filter(|&&t| t == "git").count(), 1);
        assert_eq!(tools.iter().filter(|&&t| t == "uv").count(), 1);
        // sorted, per the `for _af_c in ...` line's expectations
        let mut sorted = tools.clone();
        sorted.sort_unstable();
        assert_eq!(tools, sorted);
    }

    #[test]
    fn ensure_installed_then_is_installed_round_trips() {
        with_temp_home(|| {
            assert!(!is_installed());
            let msg = ensure_installed();
            assert!(msg.contains("written"), "unexpected message: {msg}");
            assert!(is_installed());

            // Re-running is a no-op — both blocks and BASH_ENV are already current.
            let msg2 = ensure_installed();
            assert!(
                msg2.contains("already up to date"),
                "unexpected message: {msg2}"
            );
            assert!(is_installed());
        });
    }

    #[test]
    fn ensure_installed_preserves_unrelated_bashenv_content() {
        with_temp_home(|| {
            fs::write(bashenv_path(), "# user's own stuff\nalias ll='ls -la'\n").unwrap();
            ensure_installed();
            let content = fs::read_to_string(bashenv_path()).unwrap();
            assert!(content.starts_with("# user's own stuff\nalias ll='ls -la'\n"));
            assert!(is_installed());
        });
    }

    #[test]
    fn ensure_installed_preserves_other_env_vars_in_settings() {
        with_temp_home(|| {
            let settings_path = claude_settings_path();
            fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
            fs::write(&settings_path, r#"{"env": {"OTHER_VAR": "1"}}"#).unwrap();

            ensure_installed();

            let settings: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
            assert_eq!(settings["env"]["OTHER_VAR"], "1");
            assert_eq!(settings["env"]["BASH_ENV"], bash_env_value());
        });
    }

    #[test]
    fn ensure_installed_coerces_a_non_object_env_value_instead_of_panicking() {
        with_temp_home(|| {
            let settings_path = claude_settings_path();
            fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
            fs::write(&settings_path, r#"{"env": null}"#).unwrap();

            ensure_installed();

            let settings: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
            assert_eq!(settings["env"]["BASH_ENV"], bash_env_value());
        });
    }
}
