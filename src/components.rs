// Component registry: each entry knows how to check itself and, if needed,
// fix itself. `init` runs every entry; `hook session-start` only runs the
// non-consent ones (rules/mode-pinning) since installing packages happens
// only via the explicit `init` command, never from an auto-firing hook.
use crate::jsonc::{read_json_object, write_json_pretty};
use crate::paths::{
    claude_json_path, claude_rules_dir, claude_settings_path, home, opencode_config_path,
    opencode_json_path, opencode_plugin_dir, opencode_rules_dir,
};
use crate::rule_text;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Component {
    pub id: &'static str,
    pub needs_consent: bool,
    pub describe: String,
    pub check: Box<dyn Fn() -> bool>,
    pub apply: Box<dyn Fn() -> String>,
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_default()
}

/// `doctor` builds a fresh `Component` list per host (6+ hosts by default),
/// but these three checks each spawn a subprocess (`git`, `where`/`which`)
/// and their result never depends on which host is being checked — spawning
/// them once per host multiplies process-creation overhead (dominant cost on
/// Windows) for no reason. Memoize for the process's lifetime.
fn mise_present_cached() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| crate::mise_install::mise_bin().is_some())
}

fn leanctx_installed_cached() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        crate::tool_install::installed(&crate::tool_install::LEAN_CTX)
            && crate::gateway_integrations::already_registered("leanctx")
    })
}

fn githooks_installed_cached() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| match flare_git_core::branch::repo_toplevel(&cwd()) {
        Some(root) => crate::cli::git::hooks_installed_for(&root),
        None => true,
    })
}

fn run_ok(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn claude_settings() -> Value {
    json_at(&claude_settings_path())
}

/// User-scope `claude mcp add` registrations live in `~/.claude.json`, a
/// separate file from `~/.claude/settings.json`.
fn claude_json() -> Value {
    json_at(&claude_json_path())
}

/// Removes a server entry from `~/.claude.json`'s `mcpServers` map, if
/// present — used to undo a native MCP registration another tool's own
/// installer created (e.g. lean-ctx's `onboard`) once that server has been
/// re-registered behind the agentflare gateway instead. Returns true only if
/// an entry was actually found and removed.
fn remove_claude_mcp_server(name: &str) -> bool {
    let path = claude_json_path();
    let mut root = claude_json();
    let Some(servers) = root.get_mut("mcpServers").and_then(|v| v.as_object_mut()) else {
        return false;
    };
    if servers.remove(name).is_none() {
        return false;
    }
    write_json_pretty(&path, &root).is_ok()
}

fn json_at(path: &std::path::Path) -> Value {
    crate::jsonc::read_jsonc(path, || Value::Null)
}

/// Recursively overlays `overlay` onto `base` (objects merge key-by-key;
/// anything else in `overlay` replaces `base` outright), mirroring how
/// opencode itself deep-merges `opencode.json` with `opencode.jsonc`.
fn deep_merge(base: &mut Value, overlay: &Value) {
    if let (Value::Object(base_map), Value::Object(overlay_map)) = (&mut *base, overlay) {
        for (k, v) in overlay_map {
            match base_map.get_mut(k) {
                Some(existing) => deep_merge(existing, v),
                None => {
                    base_map.insert(k.clone(), v.clone());
                }
            }
        }
    } else if !overlay.is_null() {
        *base = overlay.clone();
    }
}

/// opencode's merged view of `opencode.json` (hand-maintained) +
/// `opencode.jsonc` (agentflare-owned) — the same shape opencode itself
/// sees. Idempotency `check`s read this so a value the user already has in
/// either file isn't treated as missing; `apply` always writes only to
/// `opencode_config_path` (jsonc), never to the hand-maintained sibling.
fn opencode_config_merged() -> Value {
    let mut merged = json_at(&opencode_json_path());
    deep_merge(&mut merged, &json_at(&opencode_config_path()));
    merged
}

fn write_pinned_mode(path: &PathBuf) -> bool {
    let current: Option<String> = json_at(path)
        .get("defaultMode")
        .and_then(|m| m.as_str())
        .map(String::from);
    if current.is_some() {
        return false;
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(path, "{\"defaultMode\": \"ultra\"}\n").is_ok()
}

fn merge_json(path: &Path, root_key: &str, key: &str, value: Value) -> bool {
    let mut existing = read_json_object(path, || serde_json::json!({}));
    let obj = existing.as_object_mut().unwrap();
    let servers = obj.entry(root_key).or_insert_with(|| serde_json::json!({}));
    if let Some(m) = servers.as_object_mut() {
        m.insert(key.to_string(), value);
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    write_json_pretty(path, &existing).is_ok()
}

fn merge_opencode_mcp(path: &Path, key: &str, entry: Value) -> bool {
    let mut existing = read_json_object(path, || serde_json::json!({}));
    let obj = existing.as_object_mut().unwrap();
    let mcp = obj.entry("mcp").or_insert_with(|| serde_json::json!({}));
    if let Some(m) = mcp.as_object_mut()
        && !m.contains_key(key)
    {
        let command = entry
            .get("command")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string());
        let args: Vec<String> = entry
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let mut cmd = vec![command.unwrap_or_else(|| key.to_string())];
        cmd.extend(args);
        m.insert(
            key.to_string(),
            serde_json::json!({
                "command": cmd,
                "enabled": true,
                "type": "local",
            }),
        );
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    write_json_pretty(path, &existing).is_ok()
}

/// `opencode.jsonc` permission keys that route native reads/search/shell
/// through the flare gateway's lean-ctx tools instead — the enforcement half
/// of opencode's token-compression setup (the flare MCP entry alone only
/// makes the compact tools *available*, not preferred).
const OPENCODE_DENY_KEYS: &[&str] = &["read", "grep", "glob", "bash"];

/// True once every `OPENCODE_DENY_KEYS` entry is present (any value — an
/// existing user choice like `"ask"` counts as already decided) in the
/// merged `opencode.json` + `opencode.jsonc` view.
fn opencode_permission_configured() -> bool {
    opencode_config_merged()
        .get("permission")
        .and_then(|p| p.as_object())
        .is_some_and(|perm| OPENCODE_DENY_KEYS.iter().all(|k| perm.contains_key(*k)))
}

/// True once the `flare` MCP server is registered somewhere opencode reads
/// from — the escape hatch `opencode-token-guard` requires before it will
/// deny native read/grep/glob/bash, so denying those never strands opencode
/// without a working replacement.
fn opencode_flare_mcp_registered() -> bool {
    opencode_config_merged()
        .get("mcp")
        .and_then(|m| m.get("flare"))
        .is_some()
}

/// True once opencode has a working lean-ctx route: the `flare` MCP entry is
/// registered *and* `leanctx` is registered behind the gateway
/// (`~/.agentflare/gateway.toml`). Both are required — a `flare` entry alone
/// still gets `ServerNotFound` from `tool(action="execute", server="leanctx")`
/// if the user declined the separate `leanctx` component's consent prompt, so
/// checking `flare` in isolation would let `opencode-token-guard` deny native
/// read/grep/glob/bash with no working replacement behind them.
fn opencode_lean_ctx_route_ready() -> bool {
    opencode_flare_mcp_registered() && crate::gateway_integrations::already_registered("leanctx")
}

/// Adds any `keys` entry missing from `path`'s own `permission` object
/// (denying it), preserving whatever the user already set. Only ever
/// inserts into `opencode_config_path` (jsonc), matching `merge_opencode_mcp`.
/// Returns the number of keys actually added.
fn merge_opencode_permission(path: &Path, keys: &[&str]) -> usize {
    let mut existing = read_json_object(path, || serde_json::json!({}));
    let obj = existing.as_object_mut().unwrap();
    let permission = obj
        .entry("permission")
        .or_insert_with(|| serde_json::json!({}));
    let Some(p) = permission.as_object_mut() else {
        return 0;
    };
    let mut added = 0;
    for key in keys {
        if !p.contains_key(*key) {
            p.insert(key.to_string(), serde_json::json!("deny"));
            added += 1;
        }
    }
    if added > 0 {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = write_json_pretty(path, &existing);
    }
    added
}

fn write_if_absent(path: &PathBuf, content: &str) -> bool {
    if path.exists() {
        return false;
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(path, content).is_ok()
}

/// Per-host rule targets. Claude Code writes to its global rules folder
/// (affects every project). Everyone else has no such global folder — they
/// get project-local files instead, and only when absent, since a project
/// file is more sensitive to clobber than a per-user dotfile. Continue has
/// no dedicated rules convention (per research), so it gets none.
pub(crate) fn rule_targets(host: &str) -> Vec<(PathBuf, String)> {
    let joined = || rule_text::all().join("\n\n");
    let coaching: Vec<(String, String)> = crate::coaching::sync_targets_for_host(host);
    let joined_extra = || {
        coaching
            .iter()
            .map(|(_, body)| body.clone())
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let append_joined = |base: String| {
        if coaching.is_empty() {
            base
        } else {
            format!("{base}\n\n{}", joined_extra())
        }
    };
    match host {
        "claude-code" => {
            let dir = claude_rules_dir();
            let mut v = vec![
                (dir.join("exa.md"), rule_text::EXA.to_string()),
                (dir.join("git.md"), rule_text::GIT.to_string()),
                (dir.join("lean-ctx.md"), rule_text::LEANCTX.to_string()),
                (dir.join("flare-docs.md"), rule_text::FLARE_DOCS.to_string()),
            ];
            v.extend(
                coaching
                    .iter()
                    .map(|(id, body)| (dir.join(format!("{id}.md")), body.clone())),
            );
            v
        }
        "cursor" => {
            let content = format!("---\nalwaysApply: true\n---\n\n{}", append_joined(joined()));
            vec![(
                cwd().join(".cursor").join("rules").join("agentflare.mdc"),
                content,
            )]
        }
        "codex" => {
            let content = format!("# Rules (agentflare)\n\n{}\n", append_joined(joined()));
            vec![(cwd().join("AGENTS.md"), content)]
        }
        "windsurf" => {
            vec![(
                cwd().join(".windsurf").join("rules").join("agentflare.md"),
                append_joined(joined()) + "\n",
            )]
        }
        "vscode-copilot" => {
            vec![(
                cwd().join(".github").join("copilot-instructions.md"),
                append_joined(joined()) + "\n",
            )]
        }
        "cline" => {
            vec![(
                cwd().join(".clinerules").join("agentflare.md"),
                append_joined(joined()) + "\n",
            )]
        }
        "opencode" => {
            let dir = opencode_rules_dir();
            let mut v = vec![
                (dir.join("exa.md"), rule_text::EXA.to_string()),
                (dir.join("git.md"), rule_text::GIT.to_string()),
                (dir.join("lean-ctx.md"), rule_text::LEANCTX.to_string()),
                (dir.join("flare-docs.md"), rule_text::FLARE_DOCS.to_string()),
            ];
            v.extend(
                coaching
                    .iter()
                    .map(|(id, body)| (dir.join(format!("{id}.md")), body.clone())),
            );
            v
        }
        _ => vec![], // "continue" — no dedicated rules convention found
    }
}

/// Immediately materializes every `rule_targets(host)` entry to disk:
/// writes it if absent, refreshes it if `init::is_stale_rule` says its
/// current content is a known-old wording, otherwise leaves it alone
/// (a hand-edit, or already current). For `host == "opencode"`, also
/// re-runs the instructions-array registration.
pub(crate) fn sync_now(host: &str) -> Result<String, String> {
    let mut written = 0usize;
    let mut refreshed = 0usize;
    for (path, content) in rule_targets(host) {
        if !path.exists() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            fs::write(&path, format!("{content}\n")).map_err(|e| e.to_string())?;
            written += 1;
        } else if crate::init::is_stale_rule(&path, &content) {
            fs::write(&path, format!("{content}\n")).map_err(|e| e.to_string())?;
            refreshed += 1;
        }
    }
    if host == "opencode" {
        crate::init::wire_opencode_instructions();
    }
    Ok(format!("{written} written, {refreshed} refreshed"))
}

/// Remove a coaching rule's generated file from `host`'s rules directory.
/// Returns Ok(()) if the file didn't exist or was successfully deleted.
pub(crate) fn unsync_host(rule_id: &str, host: &str) -> Result<(), String> {
    use crate::paths::{claude_rules_dir, opencode_rules_dir};
    let dir = match host {
        "claude-code" => claude_rules_dir(),
        "opencode" => opencode_rules_dir(),
        "cursor" => cwd().join(".cursor").join("rules"),
        "codex" => cwd().join("."),
        "windsurf" => cwd().join(".windsurf").join("rules"),
        "vscode-copilot" => cwd().join(".github"),
        "cline" => cwd().join(".clinerules"),
        _ => return Ok(()),
    };
    let path = dir.join(format!("{rule_id}.md"));
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("failed to remove {path:?}: {e}"))?;
    }
    if host == "opencode" {
        crate::init::wire_opencode_instructions();
    }
    Ok(())
}

/// Agent IDs detected on this machine, for `skill_registry::Registry::open_default`'s
/// `detected_agents` param. skill-registry itself has no `agent-registry` dependency
/// (deliberately decoupled — skill discovery only needs agent IDs, not the version-
/// detection machinery), so this uses `detect_present` (PATH presence only) rather
/// than `detect_all`, which would spawn a `--version` subprocess per detected agent
/// for a value nothing here reads.
pub(crate) fn detected_skill_agents() -> Vec<String> {
    agent_registry::detect_present(agent_registry::REGISTRY)
        .into_iter()
        .map(str::to_lowercase)
        .collect()
}

/// Every skill name the shared skill_registry cache currently knows about —
/// same source `skill_search`/`skill_load` (mcp_server.rs) already serve
/// from, so "known skills" here always matches what those tools can find.
#[cfg(feature = "skill-overrides-sync")]
fn discover_skill_names() -> Result<Vec<String>, String> {
    let mut registry = skill_registry::Registry::open_default(&crate::paths::skills_db_path())
        .map_err(|e| e.to_string())?;
    registry
        .ensure_fresh(detected_skill_agents)
        .map_err(|e| e.to_string())?;
    registry.list_all_names().map_err(|e| e.to_string())
}

/// Pure merge step: adds a `"name-only"` entry for every name that doesn't
/// already have *some* skillOverrides entry — a skill the user (or another
/// tool) already set to e.g. `"off"` is left untouched. Returns how many
/// entries were newly added. Split out from `sync_skill_overrides` so this
/// logic is unit-testable without touching the real settings.json/skills.db.
#[cfg(feature = "skill-overrides-sync")]
fn apply_skill_overrides(names: &[String], settings: &mut Value) -> Result<usize, String> {
    if !settings.is_object() {
        *settings = serde_json::json!({});
    }
    let obj = settings.as_object_mut().expect("just ensured object above");
    let overrides = obj
        .entry("skillOverrides")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or("skillOverrides is not an object")?;
    let mut added = 0;
    for name in names {
        if !overrides.contains_key(name) {
            overrides.insert(name.clone(), serde_json::json!("name-only"));
            added += 1;
        }
    }
    Ok(added)
}

/// Adds a `"name-only"` entry to `~/.claude/settings.json`'s `skillOverrides`
/// for every discovered skill that doesn't already have one.
#[cfg(feature = "skill-overrides-sync")]
fn sync_skill_overrides() -> Result<usize, String> {
    let names = discover_skill_names()?;
    let path = claude_settings_path();
    let mut settings = claude_settings();
    let added = apply_skill_overrides(&names, &mut settings)?;
    if added > 0 {
        write_json_pretty(&path, &settings).map_err(|e| e.to_string())?;
    }
    Ok(added)
}

/// The core-module coaching rules agentflare ships by default: nudge the
/// flare gateway's docs/search/lean-ctx/tool-search wrappers over their
/// native equivalents. Builtin tier so `agentflare init` and every
/// non-consent SessionStart (see hook::session_start_message) refresh them
/// if their body drifts from this list; a same-id rule the user has since
/// tagged Override is left alone — override always wins.
struct DefaultCoachingRule {
    id: &'static str,
    title: &'static str,
    body: &'static str,
    tools: &'static [&'static str],
    sync: &'static [&'static str],
    enforced: bool,
}

const DEFAULT_COACHING_RULES: &[DefaultCoachingRule] = &[
    DefaultCoachingRule {
        id: "usedocs",
        title: "flare-docs for package API questions",
        body: "@rule: before writing/reviewing code against any package's API, or citing its behavior, check mcp__flare__docs (search|get) \u{2014} do not rely on memory for library API details @ecosystems: rust (docs.rs, default) \u{b7} npm (ecosystem=\"npm\") @fallback: docs tool missing from your list? ToolSearch(\"select:mcp__flare__docs\") first",
        tools: &["Edit", "Write"],
        sync: &["claude-code"],
        enforced: false,
    },
    DefaultCoachingRule {
        id: "usesearch",
        title: "flare-search over native web search",
        body: "@use: mcp__flare__search (global, 17 sources via type=) \u{2014} type=web for general internet search, plus social/news/github/academic/datasets/code/memory/store/websites/weather/financial/crypto/fx/indicators/youtube/bluesky @skip: WebFetch, WebSearch, websearch-agent, web_search_exa \u{2014} superseded by mcp__flare__search type=web @fallback: Exa MCP tools only for what flare-search has no equivalent for \u{2014} get_code_context_exa, company_research_exa",
        tools: &["WebFetch", "WebSearch"],
        sync: &["claude-code"],
        enforced: true,
    },
    DefaultCoachingRule {
        id: "useleanctx",
        title: "lean-ctx over native file/search/shell tools",
        body: "@use: lean-ctx over native tools, routed via the flare gateway \u{2014} never call mcp__lean-ctx__* directly. ctx_read>Read/cat, ctx_shell>Bash, ctx_search>Grep, ctx_glob>Glob, ctx_callgraph>grep for \"who calls X\" @call: mcp__flare__tool(action=\"execute\", server=\"leanctx\", tool=\"<name>\", args={...}) for every ctx_* op above @when: unfamiliar code \u{2014} ctx_compose FIRST, one call instead of a search->read->search chain @discover: unsure of a tools args? mcp__flare__tool(action=\"search\", query=\"<name>\") first",
        tools: &["Read", "Grep", "Glob", "Bash"],
        sync: &["claude-code"],
        enforced: false,
    },
    DefaultCoachingRule {
        id: "usetsearch",
        title: "flare tool-search over native ToolSearch for gateway tools",
        body: "@use: for leanctx's ctx_* tools \u{2014} the one namespace never in the deferred-tool list \u{2014} prefer mcp__flare__tool(action=\"search\", query=\"<what you need>\") over native ToolSearch. @note: agentflare's own first-party mcp__flare__* tools (item, asset, handoff, docs, search, memory, review, etc.) are already in the deferred-tool list \u{2014} just call them directly, no search step needed; mcp__flare__tool(action=\"search\") now finds them too as a fallback. @fallback: native ToolSearch is still right for everything else outside the gateway (WebFetch, EnterPlanMode, mcp__claude-in-chrome__*, etc.) \u{2014} this rule only blocks a ToolSearch query that actually looks like a leanctx/ctx_* lookup, not every ToolSearch call.",
        tools: &["ToolSearch"],
        sync: &["claude-code"],
        enforced: true,
    },
    DefaultCoachingRule {
        id: "yagni-gate",
        title: "YAGNI check before writing code",
        body: "@rule: before Write/Edit, run the flare-code ladder -- (1) does this need to exist at all? skip speculative features; (2) reuse an existing helper/util/pattern already in this codebase before writing a new one; (3) stdlib before custom code; (4) already-installed dependency before adding one; (5) shortest correct form wins. @why: catching over-engineering before the diff exists beats reviewing it after -- ponytail-measured ~54% avg LOC reduction, up to 94% on over-built patterns. @skip: mechanical edits (renames, formatting, config, generated files) and test/fixture code.",
        tools: &["Write", "Edit"],
        sync: &["claude-code"],
        enforced: false,
    },
];

/// True if every `DEFAULT_COACHING_RULES` entry either doesn't exist yet
/// (needs seeding), exists with content matching this list (already in
/// sync), or has been overridden by the user (tier Override — left alone).
fn coaching_defaults_satisfied() -> bool {
    let existing = crate::coaching::list_rules();
    DEFAULT_COACHING_RULES
        .iter()
        .all(|d| match existing.iter().find(|r| r.id == d.id) {
            None => false,
            Some(r) if r.tier == crate::coaching::rule::RuleTier::Override => true,
            Some(r) => r.body == d.body && r.enforced == d.enforced,
        })
}

/// Seeds any missing `DEFAULT_COACHING_RULES` entry and refreshes any
/// Builtin-tier one whose body has drifted from this list. Never touches a
/// same-id rule the user has overridden (tier Override).
fn apply_coaching_defaults() -> String {
    let existing = crate::coaching::list_rules();
    let mut changed = vec![];
    let mut failed = vec![];
    for d in DEFAULT_COACHING_RULES {
        if let Some(r) = existing.iter().find(|r| r.id == d.id)
            && (r.tier == crate::coaching::rule::RuleTier::Override
                || (r.body == d.body && r.enforced == d.enforced))
        {
            continue;
        }
        let trigger = crate::coaching::rule::RuleTrigger {
            tools: d.tools.iter().map(|s| s.to_string()).collect(),
            auto_match: true,
        };
        let sync: Vec<String> = d.sync.iter().map(|s| s.to_string()).collect();
        match crate::coaching::apply_rule(
            d.id,
            d.title,
            d.body,
            Some(trigger),
            crate::coaching::rule::RuleTier::Builtin,
            sync,
        )
        .and_then(|r| {
            if r.enforced == d.enforced {
                Ok(r)
            } else {
                crate::coaching::set_enforced(d.id, d.enforced)
            }
        }) {
            Ok(_) => changed.push(d.id),
            // Reported rather than dropped: a swallowed failure leaves
            // `coaching_defaults_satisfied` false forever, so every
            // SessionStart retries silently with nothing to explain why.
            Err(e) => failed.push(format!("{}: {e}", d.id)),
        }
    }
    if !failed.is_empty() {
        format!(
            "core-module coaching rules failed: {} (seeded/refreshed: {})",
            failed.join("; "),
            if changed.is_empty() {
                "none".to_string()
            } else {
                changed.join(", ")
            }
        )
    } else if changed.is_empty() {
        "core-module coaching rules already up to date".to_string()
    } else {
        format!(
            "core-module coaching rules seeded/refreshed: {}",
            changed.join(", ")
        )
    }
}

/// Fully-qualified flare-gateway tool names deemed safe to call unprompted.
/// Kept allowlisted in `~/.claude/settings.json` so calling them doesn't cost
/// a permission prompt every time.
///
/// `handoff` is deliberately NOT here even though most of it is local
/// item/asset writes: its `recipient="github"` path publishes a real,
/// externally-visible GitHub issue, and an allowlisted tool call skips the
/// permission prompt that would otherwise let a human catch an unintended
/// external publish before it happens.
const GATEWAY_PERMISSIONS_ALLOW: &[&str] = &[
    "mcp__flare__docs",
    "mcp__flare__search",
    "mcp__flare__tool",
    "ToolSearch",
];

/// Prefix of direct lean-ctx MCP tool names superseded by routing through
/// `mcp__flare__tool(action="execute", server="leanctx", ...)` instead.
const STALE_LEANCTX_PERMISSION_PREFIX: &str = "mcp__lean-ctx__";

/// Adds any missing `GATEWAY_PERMISSIONS_ALLOW` entry to
/// `settings.permissions.allow` and strips any direct `mcp__lean-ctx__*`
/// entry. Returns the number of entries changed.
fn apply_gateway_permissions(settings: &mut Value) -> Result<usize, String> {
    if !settings.is_object() {
        *settings = serde_json::json!({});
    }
    let obj = settings.as_object_mut().expect("just ensured object above");
    // Created when absent, but never replaced when present-and-malformed:
    // erroring out matches the `allow` handling below and keeps an unexpected
    // settings file from being destructively rewritten.
    let permissions = obj
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));
    let perm_obj = permissions
        .as_object_mut()
        .ok_or("permissions is not an object")?;
    let allow = perm_obj
        .entry("allow")
        .or_insert_with(|| serde_json::json!([]));
    let arr = allow
        .as_array_mut()
        .ok_or("permissions.allow is not an array")?;

    let before = arr.len();
    arr.retain(|v| {
        !v.as_str()
            .is_some_and(|s| s.starts_with(STALE_LEANCTX_PERMISSION_PREFIX))
    });
    let mut changed = before - arr.len();

    for name in GATEWAY_PERMISSIONS_ALLOW {
        let present = arr.iter().any(|v| v.as_str() == Some(*name));
        if !present {
            arr.push(serde_json::json!(*name));
            changed += 1;
        }
    }
    Ok(changed)
}

/// True if `~/.claude/settings.json` already allowlists every entry in
/// `GATEWAY_PERMISSIONS_ALLOW` and carries no stale direct lean-ctx entry.
fn gateway_permissions_satisfied() -> bool {
    let settings = claude_settings();
    let Some(arr) = settings
        .get("permissions")
        .and_then(|p| p.get("allow"))
        .and_then(|a| a.as_array())
    else {
        return false;
    };
    let has_all = GATEWAY_PERMISSIONS_ALLOW
        .iter()
        .all(|name| arr.iter().any(|v| v.as_str() == Some(*name)));
    let has_stale = arr.iter().any(|v| {
        v.as_str()
            .is_some_and(|s| s.starts_with(STALE_LEANCTX_PERMISSION_PREFIX))
    });
    has_all && !has_stale
}

/// Applies `apply_gateway_permissions` to the real `~/.claude/settings.json`,
/// writing back only if something changed.
fn sync_gateway_permissions() -> Result<usize, String> {
    let path = claude_settings_path();
    let mut settings = claude_settings();
    let changed = apply_gateway_permissions(&mut settings)?;
    if changed > 0 {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        write_json_pretty(&path, &settings).map_err(|e| e.to_string())?;
    }
    Ok(changed)
}
pub fn get_components(host: &str) -> Vec<Component> {
    let claude_code_only = host == "claude-code";
    let host_owned = host.to_string();
    let leanctx_log = crate::state::state_dir().join("leanctx-install.log");
    let optimize_code_config = crate::optimize::code::config_path();

    #[cfg_attr(not(feature = "skill-overrides-sync"), allow(unused_mut))]
    let mut components = vec![
        // Ahead of `rules` on purpose: `rule_targets` folds coaching-sourced
        // rules into the host rule files, and `rules` writes each file only
        // when absent. Seeded after it, these four would miss the write and
        // never make it in -- `rules`'s check passes once the files exist, so
        // there is no later pass to pick them up.
        Component {
            id: "core-coaching",
            needs_consent: false,
            describe: "built-in coaching rules nudging flare-docs, flare-search, lean-ctx, and flare tool-search over their native equivalents, plus a pre-write YAGNI check".to_string(),
            check: Box::new(move || !claude_code_only || coaching_defaults_satisfied()),
            apply: Box::new(move || {
                if !claude_code_only {
                    return "not applicable for this host".to_string();
                }
                apply_coaching_defaults()
            }),
        },
        Component {
            id: "rules",
            needs_consent: false,
            describe: format!("usage rules for {host}"),
            check: {
                let host = host_owned.clone();
                Box::new(move || {
                    let targets = rule_targets(&host);
                    !targets.is_empty() && targets.iter().all(|(p, _)| p.exists())
                })
            },
            apply: {
                let host = host_owned.clone();
                Box::new(move || {
                    let mut written = vec![];
                    for (path, content) in rule_targets(&host) {
                        if !path.exists()
                            && write_if_absent(&path, &format!("{content}\n"))
                        {
                            written.push(path.file_name().unwrap().to_string_lossy().to_string());
                        }
                    }
                    if written.is_empty() {
                        "rules already present (or none defined for this host)".to_string()
                    } else {
                        format!("rules written: {}", written.join(", "))
                    }
                })
            },
        },
        // mise (dev-tool version manager) — powers `agentflare run`'s
        // mise-wrapped agent launches (mise-managed tools on PATH for the
        // session) and any future mise-backed tool install. Host-independent.
        // (lean-ctx has its own native installer and doesn't need mise; see
        // tool_install.)
        Component {
            id: "mise",
            needs_consent: true,
            describe: "mise (dev-tool manager) — used by `agentflare run` to launch agents with mise-managed tools on PATH; https://mise.run".to_string(),
            check: Box::new(mise_present_cached),
            apply: Box::new(|| match crate::mise_install::ensure_mise() {
                crate::mise_install::MiseOutcome::Present(_) => "mise already installed".to_string(),
                crate::mise_install::MiseOutcome::Installed(p) => {
                    format!("mise installed ({p}) — open a new shell to put it on PATH")
                }
                crate::mise_install::MiseOutcome::Failed(m) => format!("mise install failed — {m}"),
            }),
        },
        // PATH shims (`~/.agentflare/shims/`): route bare tool-name calls
        // (git, npm, cargo, ...) through lean-ctx when an agent CLI is
        // active. Host-independent -- the shim binary itself gates on
        // agent-env markers at runtime, so installing it once here covers
        // every agent. No-ops (and says so) on a build that doesn't bundle
        // the shim binaries next to `agentflare` yet.
        Component {
            id: "shims",
            needs_consent: true,
            describe: "PATH shims (~/.agentflare/shims/) for git + common CLI tools — routes bare tool calls through lean-ctx while an agent CLI is active".to_string(),
            check: Box::new(crate::shim_install::all_shims_present),
            apply: Box::new(crate::shim_install::install),
        },
        // Branch-protection git hooks (.githooks/, core.hooksPath): the
        // PreToolUse guard in hook_redirect.rs only watches specific tool
        // names, so a `git commit` via Bash -- or via any tool name it
        // doesn't recognize (e.g. a gateway-routed ctx_patch call) -- slips
        // past it entirely. A native git hook is the shell-agnostic
        // enforcement boundary: it fires for every git client regardless of
        // how the commit was invoked. Host-independent (a real git hook,
        // not tied to any agent's own tool-call model), so this is not
        // gated by `claude_code_only` the way `opencode-branch-guard` is.
        Component {
            id: "githooks",
            needs_consent: true,
            describe: "Branch-protection git hooks (.githooks/, core.hooksPath) — blocks direct commits/pushes to the default branch for every git client, not just tool calls this agent's PreToolUse hook watches".to_string(),
            check: Box::new(githooks_installed_cached),
            apply: Box::new(|| match flare_git_core::branch::repo_toplevel(&cwd()) {
                Some(root) => match crate::cli::git::install_hooks_for(&root) {
                    Ok(true) => "installed .githooks/* + core.hooksPath = .githooks".to_string(),
                    Ok(false) => "already up to date".to_string(),
                    Err(e) => format!("failed: {e}"),
                },
                None => "not applicable outside a git repo".to_string(),
            }),
        },
        // Claude Code's non-interactive Bash tool sources `~/.bashenv` via
        // BASH_ENV -- the lean-ctx function dispatcher (bash-level companion
        // to the PATH shims above) and the force-push/rm -rf DEBUG-trap
        // guard both live there. Only claude-code has a BASH_ENV mechanism
        // to hook, so every other host reports satisfied.
        Component {
            id: "claude-code-bashenv-guard",
            needs_consent: true,
            describe: "~/.bashenv (BASH_ENV) — lean-ctx tool dispatch + force-push/rm -rf DEBUG-trap guardrails for Claude Code's non-interactive Bash tool".to_string(),
            check: {
                let host = host_owned.clone();
                Box::new(move || host != "claude-code" || crate::bashenv::is_installed())
            },
            apply: {
                let host = host_owned.clone();
                Box::new(move || {
                    if host != "claude-code" {
                        return "not applicable for this host".to_string();
                    }
                    crate::bashenv::ensure_installed()
                })
            },
        },
        // opencode has no PreToolUse hook of its own to wire agentflare's
        // branch guard into (that's Claude-Code-only) -- it does auto-load
        // any plugin file dropped directly in `~/.config/opencode/plugin/`,
        // so ship the same branch-guard classifier as a local plugin there.
        Component {
            id: "opencode-branch-guard",
            needs_consent: true,
            describe: "opencode branch-guard plugin (~/.config/opencode/plugin/branch-guard.js) — blocks write/edit/patch on master/main via `agentflare hook pre-tool-use`".to_string(),
            check: {
                let host = host_owned.clone();
                Box::new(move || host != "opencode" || opencode_plugin_dir().join("branch-guard.js").exists())
            },
            apply: {
                let host = host_owned.clone();
                Box::new(move || {
                    if host != "opencode" {
                        return "not applicable for this host".to_string();
                    }
                    let path = opencode_plugin_dir().join("branch-guard.js");
                    if write_if_absent(&path, rule_text::OPENCODE_BRANCH_GUARD_JS) {
                        format!("{} written", path.display())
                    } else {
                        format!("{} exists, skipped", path.display())
                    }
                })
            },
        },
        Component {
            id: "leanctx",
            needs_consent: true,
            // lean-ctx's own installer (and `onboard`) wires MCP into whichever
            // supported tool it detects natively — exactly the always-on
            // tool-list bloat the agentflare gateway exists to avoid. Right
            // after installing, register it behind the gateway instead
            // (`gateway_integrations::LEANCTX`) and, for claude-code, strip
            // whatever native entry the upstream onboarder already created so
            // the same ~80 ctx_* tools aren't declared twice.
            describe: "lean-ctx (context compression) — native installer (curl | sh, or brew), registered behind the agentflare gateway (the `tool` action-dispatch), not the host's native tool list".to_string(),
            check: Box::new(leanctx_installed_cached),
            apply: {
                let log = leanctx_log.clone();
                let host = host_owned.clone();
                Box::new(move || {
                    let mut msg = if log.exists() {
                        format!("lean-ctx install already triggered — check {}", log.display())
                    } else {
                        let _ = fs::create_dir_all(log.parent().unwrap());
                        let outcome = crate::tool_install::install(&crate::tool_install::LEAN_CTX);
                        let _ = fs::write(&log, format!("{:?}", std::time::SystemTime::now()));
                        match outcome {
                            Ok(m) => m,
                            Err(e) => return e,
                        }
                    };
                    msg = format!(
                        "{msg} + {}",
                        crate::gateway_integrations::register(&crate::gateway_integrations::LEANCTX)
                    );
                    if host == "claude-code" && remove_claude_mcp_server("lean-ctx") {
                        msg = format!("{msg} + removed native claude-code MCP entry (now gateway-only)");
                    }
                    msg
                })
            },
        },

        // agentflare's own MCP server exposes skill_search/skill_load — the
        // on-demand replacement for the always-listed skill descriptions
        // this same init wires `skillOverrides` to suppress (below). Other
        // hosts report satisfied until their MCP config format is verified here.
        Component {
            id: "agentflare-mcp",
            needs_consent: true,
            describe: if host_owned == "claude-code" {
                "agentflare MCP server (skill_search/skill_load) — claude mcp add flare -- agentflare mcp".to_string()
            } else if host_owned == "codex" {
                "agentflare MCP server (skill_search/skill_load) — codex mcp add flare -- agentflare mcp".to_string()
            } else if matches!(host_owned.as_str(), "cline" | "continue" | "opencode" | "cursor" | "windsurf" | "vscode-copilot") {
                format!("agentflare MCP server (skill_search/skill_load) — manual MCP registration for {host_owned}")
            } else {
                "agentflare MCP server — not yet supported for this host".to_string()
            },
            check: {
                let host = host_owned.clone();
                Box::new(move || match host.as_str() {
                    "claude-code" => claude_json()
                        .get("mcpServers")
                        .and_then(|m| m.get("flare"))
                        .is_some(),
                    "codex" => fs::read_to_string(home().join(".codex").join("config.toml"))
                        .map(|s| s.contains("[mcp_servers.flare]"))
                        .unwrap_or(false),
                    "cursor" => json_at(&home().join(".cursor").join("mcp.json"))
                        .get("mcpServers")
                        .and_then(|m| m.get("flare"))
                        .is_some(),
                    "windsurf" => json_at(&home().join(".codeium").join("windsurf").join("mcp_config.json"))
                        .get("mcpServers")
                        .and_then(|m| m.get("flare"))
                        .is_some(),
                    "vscode-copilot" => json_at(&cwd().join(".vscode").join("mcp.json"))
                        .get("servers")
                        .and_then(|m| m.get("flare"))
                        .is_some(),
                    "cline" => json_at(&home().join(".cline").join("mcp.json"))
                        .get("mcpServers")
                        .and_then(|m| m.get("flare"))
                        .is_some(),
                    "continue" => cwd().join(".continue").join("mcpServers").join("flare.json").exists(),
                    "opencode" => opencode_config_merged()
                        .get("mcp")
                        .and_then(|m| m.get("flare"))
                        .is_some(),
                    _ => true,
                })
            },
            apply: {
                let host = host_owned.clone();
                Box::new(move || {
                    // Register the absolute binary path, not the bare name:
                    // Claude Code launches MCP servers from its own process,
                    // which (when started from a GUI/launcher) may not have
                    // agentflare's install dir on PATH. Same reasoning as the
                    // hook wiring in init.rs.
                    let bin = crate::paths::agentflare_binary();
                    let entry = serde_json::json!({ "command": bin, "args": ["mcp"] });
                    match host.as_str() {
                        "claude-code" => {
                            // Registered as 'flare' so slash commands read
                            // /flare:artifact instead of /agentflare:artifact.
                            // Migrate: drop the legacy long-name entry first so
                            // both prefixes never coexist.
                            let _ = run_ok("claude", &["mcp", "remove", "agentflare", "-s", "user"]);
                            if run_ok("claude", &["mcp", "add", "flare", "-s", "user", "--", &bin, "mcp"]) {
                                "agentflare MCP server registered with claude-code as 'flare'".to_string()
                            } else {
                                format!("agentflare MCP registration failed — run manually: claude mcp add flare -s user -- \"{bin}\" mcp")
                            }
                        }
                        "codex" => {
                            if run_ok("codex", &["mcp", "add", "flare", "--", &bin, "mcp"]) {
                                "agentflare MCP server registered with codex as 'flare'".to_string()
                            } else {
                                format!("agentflare MCP registration failed — run manually: codex mcp add flare -- \"{bin}\" mcp")
                            }
                        }
                        "cursor" => {
                            let path = home().join(".cursor").join("mcp.json");
                            if merge_json(&path, "mcpServers", "flare", entry) {
                                format!("{} (flare registered)", path.display())
                            } else {
                                format!("failed to write {}", path.display())
                            }
                        }
                        "windsurf" => {
                            let path = home().join(".codeium").join("windsurf").join("mcp_config.json");
                            if merge_json(&path, "mcpServers", "flare", entry) {
                                format!("{} (flare registered)", path.display())
                            } else {
                                format!("failed to write {}", path.display())
                            }
                        }
                        "vscode-copilot" => {
                            let mut entry = entry;
                            if let Some(obj) = entry.as_object_mut() {
                                obj.insert("type".to_string(), serde_json::Value::String("stdio".to_string()));
                            }
                            let path = cwd().join(".vscode").join("mcp.json");
                            if merge_json(&path, "servers", "flare", entry) {
                                format!("{} (flare registered)", path.display())
                            } else {
                                format!("failed to write {}", path.display())
                            }
                        }
                        "cline" => {
                            let path = home().join(".cline").join("mcp.json");
                            if merge_json(&path, "mcpServers", "flare", entry) {
                                format!("{} (flare registered)", path.display())
                            } else {
                                format!("failed to write {}", path.display())
                            }
                        }
                        "continue" => {
                            let path = cwd().join(".continue").join("mcpServers").join("flare.json");
                            if write_if_absent(&path, &(serde_json::to_string_pretty(&entry).unwrap() + "\n")) {
                                format!("{} written", path.display())
                            } else {
                                format!("{} exists, skipped", path.display())
                            }
                        }
                        "opencode" => {
                            let path = opencode_config_path();
                            if merge_opencode_mcp(&path, "flare", entry) {
                                format!("{} (flare registered)", path.display())
                            } else {
                                format!("failed to write {}", path.display())
                            }
                        }
                        _ => format!("no agentflare MCP integration defined for host '{host}'"),
                    }
                })
            },
        },
        // Registering the flare MCP entry (above) only makes opencode's
        // gateway-fronted lean-ctx tools available; without this, opencode
        // still defaults to its native read/grep/glob/bash, so a
        // dispatch-driven agent (e.g. the SDD-loop implementer) gets no
        // token compression unless someone happened to run `lean-ctx
        // onboard` by hand on that host. Denying the native equivalents
        // makes the compact path the only path — but only once `flare` is
        // actually registered, so this never strands opencode without a
        // working read/shell route.
        Component {
            id: "opencode-token-guard",
            needs_consent: true,
            describe: "opencode.jsonc permission block (read/grep/glob/bash: deny) — routes file reads and shell commands through the flare gateway's lean-ctx tools instead of native calls".to_string(),
            check: {
                let host = host_owned.clone();
                Box::new(move || host != "opencode" || opencode_permission_configured())
            },
            apply: {
                let host = host_owned.clone();
                Box::new(move || {
                    if host != "opencode" {
                        return "not applicable for this host".to_string();
                    }
                    if !opencode_lean_ctx_route_ready() {
                        return "skipped — flare MCP entry and/or the leanctx gateway registration aren't wired yet in opencode's config; re-run `agentflare init` once both are set up (denying native tools without a working MCP alternative would strand opencode with no read/shell path)".to_string();
                    }
                    let path = opencode_config_path();
                    let added = merge_opencode_permission(&path, OPENCODE_DENY_KEYS);
                    if added > 0 {
                        format!("{} ({added} permission key(s) set to deny: read/grep/glob/bash)", path.display())
                    } else {
                        format!("{} already has all permission keys configured", path.display())
                    }
                })
            },
        },
        Component {
            id: "optimize-code-mode",
            needs_consent: false,
            describe: "pin flare code to ultra mode".to_string(),
            check: {
                let path = optimize_code_config.clone();
                Box::new(move || {
                    if !claude_code_only {
                        return true;
                    }
                    json_at(&path).get("defaultMode").is_some()
                })
            },
            apply: {
                let path = optimize_code_config.clone();
                Box::new(move || {
                    if write_pinned_mode(&path) {
                        "flare code pinned to ultra".to_string()
                    } else {
                        "flare code mode already set".to_string()
                    }
                })
            },
        },
        Component {
            id: "gateway-permissions",
            needs_consent: false,
            describe: "allowlist the flare gateway tools (mcp__flare__docs, mcp__flare__search, mcp__flare__tool, ToolSearch) in ~/.claude/settings.json and remove superseded direct mcp__lean-ctx__* entries".to_string(),
            check: Box::new(move || !claude_code_only || gateway_permissions_satisfied()),
            apply: Box::new(move || {
                if !claude_code_only {
                    return "not applicable for this host".to_string();
                }
                match sync_gateway_permissions() {
                    Ok(0) => "gateway permissions already up to date".to_string(),
                    Ok(n) => format!("gateway permissions updated ({n} change(s))"),
                    Err(e) => format!("gateway permissions sync failed: {e}"),
                }
            }),
        },
    ];

    // Gated behind the `skill-overrides-sync` cargo feature (off by
    // default, not part of released builds) until we have real evidence
    // this saves money rather than just cache-cheap context tokens (measured
    // ~900 tokens/turn of context-window space, mostly cache reads).
    // Suppresses every known skill's description from
    // Claude Code's always-on listing (settings.json `skillOverrides:
    // name-only`) — names stay typable, skill_search/skill_load
    // (registered above) become the on-demand detail source.
    // Claude-Code-only: other hosts have no equivalent per-skill override
    // mechanism. Not consent-gated (a local config tweak, same trust
    // level as optimize-code-mode above) so it also re-syncs on
    // every session-start as new skills appear, not just during `init`.
    #[cfg(feature = "skill-overrides-sync")]
    {
        let host = host_owned.clone();
        components.push(Component {
            id: "skill-overrides-sync",
            needs_consent: false,
            describe: "sync skillOverrides so newly-discovered skills defer their description to on-demand search".to_string(),
            check: Box::new(move || {
                if host != "claude-code" {
                    return true;
                }
                let Ok(names) = discover_skill_names() else { return true };
                let settings = claude_settings();
                let overrides = settings.get("skillOverrides").and_then(|v| v.as_object());
                names.iter().all(|n| overrides.is_some_and(|o| o.contains_key(n)))
            }),
            apply: Box::new(|| match sync_skill_overrides() {
                Ok(0) => "skillOverrides already up to date".to_string(),
                Ok(n) => format!("skillOverrides: {n} skill(s) set to name-only"),
                Err(e) => format!("skillOverrides sync failed: {e}"),
            }),
        });
    }

    components
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOSTS: &[&str] = &[
        "claude-code",
        "codex",
        "cursor",
        "windsurf",
        "vscode-copilot",
        "cline",
        "continue",
        "opencode",
    ];

    #[test]
    fn every_host_gets_the_full_component_set() {
        // "skill-overrides-sync" only exists behind the `skill-overrides-sync`
        // cargo feature (opt-in, not part of released builds — unconfirmed
        // $ savings, see Cargo.toml). Expected ids adjust accordingly.
        #[cfg(not(feature = "skill-overrides-sync"))]
        let expected: Vec<&str> = vec![
            "core-coaching",
            "rules",
            "mise",
            "shims",
            "githooks",
            "claude-code-bashenv-guard",
            "opencode-branch-guard",
            "leanctx",
            "agentflare-mcp",
            "opencode-token-guard",
            "optimize-code-mode",
            "gateway-permissions",
        ];
        #[cfg(feature = "skill-overrides-sync")]
        let expected: Vec<&str> = vec![
            "core-coaching",
            "rules",
            "mise",
            "shims",
            "githooks",
            "claude-code-bashenv-guard",
            "opencode-branch-guard",
            "leanctx",
            "agentflare-mcp",
            "optimize-code-mode",
            "gateway-permissions",
            "skill-overrides-sync",
        ];

        for host in HOSTS {
            let components = get_components(host);
            assert_eq!(
                components.len(),
                expected.len(),
                "expected {} components for host '{host}', got {}",
                expected.len(),
                components.len()
            );
            let ids: Vec<_> = components.iter().map(|c| c.id).collect();
            assert_eq!(ids, expected);
        }
    }

    #[test]
    fn rule_targets_are_project_local_except_claude_code_and_opencode() {
        // claude-code writes to the global rules dir under ~/.claude/rules.
        let cc_targets = rule_targets("claude-code");
        assert!(!cc_targets.is_empty());
        for (path, _) in &cc_targets {
            assert!(path.to_string_lossy().contains(".claude"));
        }

        // opencode writes to the global rules dir under ~/.config/opencode/rules.
        let oc_targets = rule_targets("opencode");
        assert!(!oc_targets.is_empty());
        for (path, _) in &oc_targets {
            assert!(path.to_string_lossy().contains("opencode"));
        }

        // Every other defined host writes a project-local path — check for
        // the actual per-host marker dir, not "starts with home" (the repo
        // itself can live under home, which would make that check useless).
        let expectations = [
            ("cursor", ".cursor"),
            ("codex", "AGENTS.md"),
            ("windsurf", ".windsurf"),
            ("vscode-copilot", ".github"),
            ("cline", ".clinerules"),
        ];
        for (host, marker) in expectations {
            let targets = rule_targets(host);
            assert!(!targets.is_empty(), "expected rule targets for '{host}'");
            for (path, _) in &targets {
                assert!(
                    path.to_string_lossy().contains(marker),
                    "'{host}' rule target {path:?} should contain '{marker}'"
                );
            }
        }

        // "continue" has no dedicated rules convention — empty on purpose.
        assert!(rule_targets("continue").is_empty());
    }

    #[test]
    fn flare_docs_rule_is_written_for_claude_code_and_opencode() {
        let cc_targets = rule_targets("claude-code");
        let cc_flare_docs = cc_targets
            .iter()
            .find(|(path, _)| path.to_string_lossy().ends_with("flare-docs.md"));
        assert!(
            cc_flare_docs.is_some(),
            "claude-code rule_targets should include flare-docs.md"
        );
        assert_eq!(cc_flare_docs.unwrap().1, rule_text::FLARE_DOCS);

        let oc_targets = rule_targets("opencode");
        assert!(
            oc_targets
                .iter()
                .any(|(path, _)| path.to_string_lossy().ends_with("flare-docs.md")),
            "opencode rule_targets should include flare-docs.md"
        );
    }

    #[test]
    fn flare_docs_rule_is_included_in_joined_hosts() {
        // Hosts that concatenate rule_text::all() (cursor, codex, windsurf,
        // vscode-copilot, cline) should pick up FLARE_DOCS automatically
        // once it's added to all() -- no per-host wiring needed for those.
        for host in ["cursor", "codex", "windsurf", "vscode-copilot", "cline"] {
            let targets = rule_targets(host);
            assert!(
                targets
                    .iter()
                    .any(|(_, content)| content.contains(rule_text::FLARE_DOCS)),
                "'{host}' joined rule content should include the flare-docs rule text"
            );
        }
    }

    #[test]
    fn rule_targets_includes_coaching_sourced_rule_for_claude_code_and_opencode() {
        crate::paths::test_support::with_temp_home(|| {
            crate::coaching::apply_rule(
                "search17",
                "T",
                "Coaching body",
                None,
                crate::coaching::rule::RuleTier::Builtin,
                vec!["claude-code".to_string(), "opencode".to_string()],
            )
            .unwrap();

            let cc = rule_targets("claude-code");
            assert!(
                cc.iter()
                    .any(|(p, c)| p.to_string_lossy().ends_with("search17.md")
                        && c == "Coaching body")
            );

            let oc = rule_targets("opencode");
            assert!(
                oc.iter()
                    .any(|(p, c)| p.to_string_lossy().ends_with("search17.md")
                        && c == "Coaching body")
            );
        });
    }

    #[test]
    fn rule_targets_appends_coaching_sourced_body_into_joined_hosts() {
        crate::paths::test_support::with_temp_home(|| {
            crate::coaching::apply_rule(
                "search17",
                "T",
                "Coaching body",
                None,
                crate::coaching::rule::RuleTier::Builtin,
                vec!["cursor".to_string()],
            )
            .unwrap();

            let targets = rule_targets("cursor");
            assert_eq!(targets.len(), 1, "cursor stays a single joined file");
            assert!(targets[0].1.contains("Coaching body"));
            assert!(
                targets[0].1.contains(rule_text::FLARE_DOCS),
                "existing builtin content must still be present"
            );
        });
    }

    #[test]
    fn rule_targets_omits_coaching_rule_not_synced_to_this_host() {
        crate::paths::test_support::with_temp_home(|| {
            crate::coaching::apply_rule(
                "search17",
                "T",
                "Coaching body",
                None,
                crate::coaching::rule::RuleTier::Builtin,
                vec!["opencode".to_string()],
            )
            .unwrap();

            let cc = rule_targets("claude-code");
            assert!(!cc.iter().any(|(_, c)| c == "Coaching body"));
        });
    }

    #[test]
    fn agentflare_mcp_check_reflects_codex_config_toml_substring() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("codex");
            let agentflare_mcp = components
                .iter()
                .find(|c| c.id == "agentflare-mcp")
                .unwrap();
            assert!(
                !(agentflare_mcp.check)(),
                "no config.toml yet — should not be satisfied"
            );

            let config = home().join(".codex").join("config.toml");
            fs::create_dir_all(config.parent().unwrap()).unwrap();
            // unrelated entry present — must not false-positive
            fs::write(&config, "[mcp_servers.other]\ncommand = \"foo\"\n").unwrap();
            assert!(
                !(agentflare_mcp.check)(),
                "unrelated entry must not satisfy the check"
            );

            fs::write(&config, "[mcp_servers.other]\ncommand = \"foo\"\n\n[mcp_servers.flare]\ncommand = \"agentflare\"\n").unwrap();
            assert!(
                (agentflare_mcp.check)(),
                "flare entry present — should be satisfied"
            );
        });
    }

    #[test]
    fn agentflare_mcp_cursor_check_then_apply_then_check() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("cursor");
            let agentflare_mcp = components
                .iter()
                .find(|c| c.id == "agentflare-mcp")
                .unwrap();
            assert!(!(agentflare_mcp.check)());
            (agentflare_mcp.apply)();
            assert!((agentflare_mcp.check)());
        });
    }

    #[test]
    fn agentflare_mcp_cursor_apply_does_not_clobber_existing_servers() {
        crate::paths::test_support::with_temp_home(|| {
            let path = home().join(".cursor").join("mcp.json");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, r#"{"mcpServers": {"other": {"command": "foo"}}}"#).unwrap();

            let components = get_components("cursor");
            let agentflare_mcp = components
                .iter()
                .find(|c| c.id == "agentflare-mcp")
                .unwrap();
            (agentflare_mcp.apply)();

            let value = json_at(&path);
            assert!(
                value["mcpServers"]["other"].is_object(),
                "existing entry must survive"
            );
            assert!(
                value["mcpServers"]["flare"].is_object(),
                "flare entry must be added"
            );
        });
    }

    #[test]
    fn agentflare_mcp_opencode_apply_survives_jsonc_comments_and_trailing_comma() {
        crate::paths::test_support::with_temp_home(|| {
            let path = home()
                .join(".config")
                .join("opencode")
                .join("opencode.jsonc");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            // Real jsonc: comments + trailing comma + an existing mcp server
            // entry that must survive. Before the jsonc parser, this parse
            // failure was treated as "no existing config" and everything
            // below (including `other-server`) was silently dropped on write.
            fs::write(
                &path,
                r#"{
  // OpenCode configuration
  "mcp": {
    "other-server": { "type": "local", "command": ["other-server"] },
  },
}"#,
            )
            .unwrap();

            let components = get_components("opencode");
            let agentflare_mcp = components
                .iter()
                .find(|c| c.id == "agentflare-mcp")
                .unwrap();
            (agentflare_mcp.apply)();

            let value = json_at(&path);
            assert!(
                value["mcp"]["other-server"].is_object(),
                "existing entry must survive"
            );
            assert!(
                value["mcp"]["flare"].is_object(),
                "flare entry must be added"
            );
        });
    }

    #[test]
    fn agentflare_mcp_windsurf_check_then_apply_then_check() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("windsurf");
            let agentflare_mcp = components
                .iter()
                .find(|c| c.id == "agentflare-mcp")
                .unwrap();
            assert!(!(agentflare_mcp.check)());
            (agentflare_mcp.apply)();
            assert!((agentflare_mcp.check)());
        });
    }

    #[test]
    fn agentflare_mcp_vscode_copilot_check_then_apply_then_check() {
        crate::paths::test_support::with_temp_cwd(|| {
            let components = get_components("vscode-copilot");
            let agentflare_mcp = components
                .iter()
                .find(|c| c.id == "agentflare-mcp")
                .unwrap();
            assert!(!(agentflare_mcp.check)());
            (agentflare_mcp.apply)();
            assert!((agentflare_mcp.check)());
        });
    }

    #[test]
    fn agentflare_mcp_vscode_copilot_writes_servers_key_with_stdio_type() {
        crate::paths::test_support::with_temp_cwd(|| {
            let components = get_components("vscode-copilot");
            let agentflare_mcp = components
                .iter()
                .find(|c| c.id == "agentflare-mcp")
                .unwrap();
            (agentflare_mcp.apply)();

            let path = cwd().join(".vscode").join("mcp.json");
            let value = json_at(&path);
            assert!(
                value.get("mcpServers").is_none(),
                "must use 'servers', not 'mcpServers'"
            );
            assert_eq!(value["servers"]["flare"]["type"], "stdio");
        });
    }

    #[test]
    #[cfg(feature = "skill-overrides-sync")]
    fn skill_overrides_sync_reports_satisfied_on_non_claude_code_hosts() {
        for host in [
            "codex",
            "cursor",
            "windsurf",
            "vscode-copilot",
            "cline",
            "continue",
            "opencode",
        ] {
            let components = get_components(host);
            let sync = components
                .iter()
                .find(|c| c.id == "skill-overrides-sync")
                .unwrap();
            assert!(
                (sync.check)(),
                "skill-overrides-sync should be satisfied on '{host}'"
            );
        }
    }

    #[test]
    #[cfg(feature = "skill-overrides-sync")]
    fn apply_skill_overrides_adds_name_only_for_new_skills_only() {
        let mut settings = serde_json::json!({
            "skillOverrides": { "already-configured": "off" }
        });
        let names = vec!["already-configured".to_string(), "brand-new".to_string()];
        let added = apply_skill_overrides(&names, &mut settings).unwrap();
        assert_eq!(added, 1);
        assert_eq!(settings["skillOverrides"]["already-configured"], "off");
        assert_eq!(settings["skillOverrides"]["brand-new"], "name-only");
    }

    #[test]
    #[cfg(feature = "skill-overrides-sync")]
    fn apply_skill_overrides_handles_missing_settings_object() {
        let mut settings = Value::Null;
        let added = apply_skill_overrides(&["some-skill".to_string()], &mut settings).unwrap();
        assert_eq!(added, 1);
        assert_eq!(settings["skillOverrides"]["some-skill"], "name-only");
    }

    #[test]
    #[cfg(feature = "skill-overrides-sync")]
    fn apply_skill_overrides_is_idempotent() {
        let mut settings = serde_json::json!({});
        let names = vec!["skill-a".to_string()];
        assert_eq!(apply_skill_overrides(&names, &mut settings).unwrap(), 1);
        assert_eq!(apply_skill_overrides(&names, &mut settings).unwrap(), 0);
    }

    #[test]
    fn remove_claude_mcp_server_removes_only_the_named_entry() {
        crate::paths::test_support::with_temp_home(|| {
            let path = home().join(".claude.json");
            fs::write(
                &path,
                serde_json::json!({
                    "mcpServers": {
                        "lean-ctx": {"command": "lean-ctx"},
                        "flare": {"command": "agentflare"}
                    }
                })
                .to_string(),
            )
            .unwrap();

            assert!(remove_claude_mcp_server("lean-ctx"));

            let value = json_at(&path);
            assert!(value["mcpServers"]["lean-ctx"].is_null());
            assert!(value["mcpServers"]["flare"].is_object());
        });
    }

    #[test]
    fn remove_claude_mcp_server_is_a_noop_when_absent() {
        crate::paths::test_support::with_temp_home(|| {
            assert!(!remove_claude_mcp_server("lean-ctx"));
        });
    }

    #[test]
    fn opencode_config_merged_sees_both_files() {
        crate::paths::test_support::with_temp_home(|| {
            fs::create_dir_all(opencode_json_path().parent().unwrap()).unwrap();
            fs::write(
                opencode_json_path(),
                r#"{"mcp": {"other": {"command": "foo"}}, "plugin": ["a.js"]}"#,
            )
            .unwrap();
            fs::write(
                opencode_config_path(),
                r#"{"mcp": {"flare": {"command": "agentflare"}}}"#,
            )
            .unwrap();

            let merged = opencode_config_merged();
            assert!(merged["mcp"]["other"].is_object());
            assert!(merged["mcp"]["flare"].is_object());
            assert_eq!(merged["plugin"][0], "a.js");
        });
    }

    #[test]
    fn agentflare_mcp_opencode_check_sees_flare_entry_in_sibling_json_file() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("opencode");
            let agentflare_mcp = components
                .iter()
                .find(|c| c.id == "agentflare-mcp")
                .unwrap();
            assert!(!(agentflare_mcp.check)());

            fs::create_dir_all(opencode_json_path().parent().unwrap()).unwrap();
            fs::write(
                opencode_json_path(),
                r#"{"mcp": {"flare": {"command": "agentflare"}}}"#,
            )
            .unwrap();

            assert!(
                (agentflare_mcp.check)(),
                "flare entry in opencode.json (not just opencode.jsonc) should satisfy the check"
            );
        });
    }

    #[test]
    fn opencode_branch_guard_check_then_apply_then_check() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("opencode");
            let guard = components
                .iter()
                .find(|c| c.id == "opencode-branch-guard")
                .unwrap();
            assert!(!(guard.check)());
            (guard.apply)();
            assert!((guard.check)());
            assert_eq!(
                fs::read_to_string(opencode_plugin_dir().join("branch-guard.js")).unwrap(),
                rule_text::OPENCODE_BRANCH_GUARD_JS
            );
        });
    }

    #[test]
    fn opencode_branch_guard_is_satisfied_for_non_opencode_hosts() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("claude-code");
            let guard = components
                .iter()
                .find(|c| c.id == "opencode-branch-guard")
                .unwrap();
            assert!((guard.check)());
        });
    }

    #[test]
    fn claude_code_bashenv_guard_check_then_apply_then_check() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("claude-code");
            let guard = components
                .iter()
                .find(|c| c.id == "claude-code-bashenv-guard")
                .unwrap();
            assert!(!(guard.check)());
            (guard.apply)();
            assert!((guard.check)());
        });
    }

    #[test]
    fn claude_code_bashenv_guard_is_satisfied_for_non_claude_code_hosts() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("opencode");
            let guard = components
                .iter()
                .find(|c| c.id == "claude-code-bashenv-guard")
                .unwrap();
            assert!((guard.check)());
        });
    }

    #[test]
    fn apply_gateway_permissions_adds_missing_and_strips_stale_leanctx() {
        let mut settings = serde_json::json!({
            "permissions": {
                "allow": ["mcp__lean-ctx__ctx_read", "mcp__flare__docs"]
            }
        });
        let changed = apply_gateway_permissions(&mut settings).unwrap();
        assert_eq!(
            changed, 4,
            "3 missing entries added + 1 stale entry stripped"
        );
        let allow = settings["permissions"]["allow"].as_array().unwrap();
        for name in GATEWAY_PERMISSIONS_ALLOW {
            assert!(
                allow.iter().any(|v| v.as_str() == Some(*name)),
                "expected {name} in permissions.allow"
            );
        }
        assert!(
            allow.iter().all(|v| !v
                .as_str()
                .unwrap()
                .starts_with(STALE_LEANCTX_PERMISSION_PREFIX)),
            "no direct mcp__lean-ctx__* entry should remain"
        );
    }

    #[test]
    fn apply_gateway_permissions_is_idempotent_once_satisfied() {
        let mut settings = serde_json::json!({});
        apply_gateway_permissions(&mut settings).unwrap();
        let changed = apply_gateway_permissions(&mut settings).unwrap();
        assert_eq!(
            changed, 0,
            "second pass over already-correct settings changes nothing"
        );
    }

    // Rewriting a settings file we cannot parse would silently discard
    // whatever the user actually had there.
    #[test]
    fn apply_gateway_permissions_errors_on_malformed_permissions_rather_than_clobbering() {
        let mut settings = serde_json::json!({ "permissions": "not an object" });
        assert!(apply_gateway_permissions(&mut settings).is_err());
        assert_eq!(settings["permissions"], "not an object", "left untouched");
    }

    #[test]
    fn gateway_permissions_component_check_then_apply_then_check() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("claude-code");
            let gw = components
                .iter()
                .find(|c| c.id == "gateway-permissions")
                .unwrap();
            assert!(!(gw.check)(), "fresh temp home has no settings.json yet");
            (gw.apply)();
            assert!((gw.check)(), "should be satisfied immediately after apply");
        });
    }

    #[test]
    fn gateway_permissions_is_satisfied_for_non_claude_code_hosts() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("opencode");
            let gw = components
                .iter()
                .find(|c| c.id == "gateway-permissions")
                .unwrap();
            assert!((gw.check)());
        });
    }

    #[test]
    fn opencode_token_guard_is_satisfied_for_non_opencode_hosts() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("claude-code");
            let guard = components
                .iter()
                .find(|c| c.id == "opencode-token-guard")
                .unwrap();
            assert!((guard.check)());
        });
    }

    #[test]
    fn opencode_token_guard_refuses_to_deny_native_tools_without_flare_registered() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("opencode");
            let guard = components
                .iter()
                .find(|c| c.id == "opencode-token-guard")
                .unwrap();
            assert!(!(guard.check)(), "fresh temp home has no config yet");

            let msg = (guard.apply)();
            assert!(
                msg.contains("skipped"),
                "must not deny native tools before flare MCP is registered: {msg}"
            );
            assert!(
                !(guard.check)(),
                "check should still fail — nothing was written"
            );
            assert!(
                !opencode_config_path().exists(),
                "apply must not create opencode.jsonc when it skips"
            );
        });
    }

    #[test]
    fn opencode_token_guard_refuses_to_deny_native_tools_without_leanctx_registered() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("opencode");
            let mcp = components
                .iter()
                .find(|c| c.id == "agentflare-mcp")
                .unwrap();
            (mcp.apply)();

            let guard = components
                .iter()
                .find(|c| c.id == "opencode-token-guard")
                .unwrap();
            let msg = (guard.apply)();
            assert!(
                msg.contains("skipped"),
                "flare alone isn't enough — leanctx isn't registered behind the gateway yet: {msg}"
            );
            assert!(!(guard.check)());
        });
    }

    #[test]
    fn opencode_token_guard_check_then_apply_then_check_once_flare_is_registered() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("opencode");
            let mcp = components
                .iter()
                .find(|c| c.id == "agentflare-mcp")
                .unwrap();
            (mcp.apply)();
            crate::gateway_integrations::register(&crate::gateway_integrations::LEANCTX);

            let guard = components
                .iter()
                .find(|c| c.id == "opencode-token-guard")
                .unwrap();
            assert!(!(guard.check)());
            let msg = (guard.apply)();
            assert!(!msg.contains("skipped"), "unexpected skip: {msg}");
            assert!(
                (guard.check)(),
                "should be satisfied immediately after apply"
            );

            let parsed =
                crate::jsonc::read_jsonc(&opencode_config_path(), || serde_json::Value::Null);
            for key in OPENCODE_DENY_KEYS {
                assert_eq!(parsed["permission"][key], "deny");
            }
        });
    }

    #[test]
    fn opencode_token_guard_preserves_an_existing_permission_choice() {
        crate::paths::test_support::with_temp_home(|| {
            fs::create_dir_all(opencode_config_path().parent().unwrap()).unwrap();
            fs::write(
                opencode_config_path(),
                r#"{"permission": {"read": "ask"}, "mcp": {"flare": {"command": "agentflare"}}}"#,
            )
            .unwrap();
            crate::gateway_integrations::register(&crate::gateway_integrations::LEANCTX);

            let components = get_components("opencode");
            let guard = components
                .iter()
                .find(|c| c.id == "opencode-token-guard")
                .unwrap();
            (guard.apply)();

            let written = fs::read_to_string(opencode_config_path()).unwrap();
            assert!(
                written.contains("\"read\": \"ask\""),
                "must not override a value the user already set: {written}"
            );
            assert!(written.contains("\"grep\": \"deny\""));
        });
    }

    // No with_temp_cwd-based check/apply-cycle test here (unlike the other
    // components above): the githooks component's closures resolve
    // `flare_git_core::branch::repo_toplevel(&cwd())` fresh on every call,
    // and under cargo test's parallel execution that raced with this
    // process's real cwd and mutated *this actual checkout*'s
    // core.hooksPath instead of the isolated tempdir (confirmed via
    // .git/config's mtime). `hooks_installed_for`/`install_hooks_for`
    // (`cli::git`'s tests) already cover the exact same logic these
    // closures just delegate to, with explicit repo_root paths instead of
    // ambient cwd -- no coverage lost by not re-testing it here too.

    #[test]
    fn coaching_defaults_seed_all_default_rules_on_fresh_home() {
        crate::paths::test_support::with_temp_home(|| {
            assert!(!coaching_defaults_satisfied());
            let summary = apply_coaching_defaults();
            assert!(summary.contains("seeded/refreshed"));
            assert!(coaching_defaults_satisfied());

            let existing = crate::coaching::list_rules();
            for d in DEFAULT_COACHING_RULES {
                let r = existing
                    .iter()
                    .find(|r| r.id == d.id)
                    .unwrap_or_else(|| panic!("expected seeded rule '{}'", d.id));
                assert_eq!(r.body, d.body);
                assert_eq!(r.tier, crate::coaching::rule::RuleTier::Builtin);
                assert_eq!(r.enforced, d.enforced);
                let got_tools: Vec<&str> = r
                    .trigger
                    .as_ref()
                    .map(|t| t.tools.iter().map(String::as_str).collect())
                    .unwrap_or_default();
                assert_eq!(
                    got_tools, d.tools,
                    "rule '{}' tool trigger drifted from its default",
                    d.id
                );
                let got_sync: Vec<&str> = r.sync.iter().map(String::as_str).collect();
                assert_eq!(
                    got_sync, d.sync,
                    "rule '{}' sync target drifted from its default",
                    d.id
                );
            }
        });
    }

    #[test]
    fn coaching_defaults_apply_is_idempotent_once_seeded() {
        crate::paths::test_support::with_temp_home(|| {
            apply_coaching_defaults();
            let summary = apply_coaching_defaults();
            assert_eq!(summary, "core-module coaching rules already up to date");
        });
    }

    #[test]
    fn coaching_defaults_never_overwrite_a_user_override() {
        crate::paths::test_support::with_temp_home(|| {
            apply_coaching_defaults();

            let first = DEFAULT_COACHING_RULES.first().unwrap();
            crate::coaching::apply_rule(
                first.id,
                "my own title",
                "my own customized body",
                None,
                crate::coaching::rule::RuleTier::Override,
                vec![],
            )
            .unwrap();

            assert!(
                coaching_defaults_satisfied(),
                "an Override-tier rule at a default id counts as satisfied"
            );
            apply_coaching_defaults();

            let existing = crate::coaching::list_rules();
            let r = existing.iter().find(|r| r.id == first.id).unwrap();
            assert_eq!(r.body, "my own customized body");
            assert_eq!(r.tier, crate::coaching::rule::RuleTier::Override);
        });
    }

    #[test]
    fn coaching_defaults_component_check_then_apply_then_check() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("claude-code");
            let cc = components.iter().find(|c| c.id == "core-coaching").unwrap();
            assert!(!(cc.check)());
            (cc.apply)();
            assert!((cc.check)());
        });
    }

    #[test]
    fn coaching_defaults_are_satisfied_for_non_claude_code_hosts() {
        crate::paths::test_support::with_temp_home(|| {
            let components = get_components("opencode");
            let cc = components.iter().find(|c| c.id == "core-coaching").unwrap();
            assert!((cc.check)());
        });
    }

    // The seeded rules reach the host rule files only if they exist before
    // `rules` writes them, and `rules` writes each file just once.
    #[test]
    fn core_coaching_is_ordered_before_the_rules_component() {
        let ids: Vec<&str> = get_components("claude-code").iter().map(|c| c.id).collect();
        let cc = ids.iter().position(|i| *i == "core-coaching").unwrap();
        let rules = ids.iter().position(|i| *i == "rules").unwrap();
        assert!(cc < rules, "core-coaching must be applied before rules");
    }
}
