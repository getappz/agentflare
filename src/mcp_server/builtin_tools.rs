//! Static catalog of agentflare's own first-party MCP tools, so
//! `mcp__flare__tool(action="search")` can actually answer a query like
//! "item" or "handoff" instead of falling straight through to the public
//! MCP Registry fallback and returning unrelated third-party servers.
//!
//! `gateway_registry::Registry::search` only indexes *downstream* servers
//! configured in `gateway.toml` (e.g. leanctx) — agentflare's own tools
//! (this file, `item`/`asset`/`handoff`/...) were never in that index, so
//! any query for one of them had zero local hits and fell back to
//! `registry_search::search_registry`, which queries the public MCP
//! Registry and has no notion of agentflare's own tool surface at all.
//!
//! Kept in sync by hand with each tool's own `#[tool(description = ...)]`
//! in `mcp_server.rs` — a short, slow-changing list, not worth codegening.

/// (bare tool name, description) — bare name matches how `#[tool]` methods
/// are named; callers reach these as `mcp__flare__<name>`, never through
/// `tool(action="execute", ...)` (that action is for *downstream* gateway
/// servers only, agentflare's own tools aren't registered as one).
const BUILTIN_TOOLS: &[(&str, &str)] = &[
    (
        "check_session_health",
        "Check if a session should be refreshed based on turn count and elapsed time.",
    ),
    (
        "optimize_instructions",
        "Get the flare-code (code-minimalism / lazy-senior-dev) instructions for a mode, as structured {mode, instructions}. Read-only — does not change the active mode. Omit `mode` for the active/default one.",
    ),
    (
        "get_routing_suggestion",
        "Get a model routing suggestion for a given prompt.",
    ),
    (
        "skill_detect",
        "Detect intent from user prompt and find relevant skills. Returns intent classification + ranked skill matches.",
    ),
    (
        "skill",
        "Skill operations — search installed skills or load one by name. Single consolidated tool with `action` field (search|load).",
    ),
    (
        "artifact",
        "Artifact operations — publish, list, get, diff, search, or delete. Single consolidated tool with `action` field (delete|diff|get|list|publish|search).",
    ),
    (
        "handoff",
        "Hand a work product to another agent: assigns/creates an item for the recipient (in the repo's linked project) and attaches the content to it as an asset.",
    ),
    (
        "channel_send",
        "Send a text message out to a chat platform (telegram, slack, or discord). The bot token must already be stored as the vault secret '<platform>_bot_token' (agentflare vault set <platform>_bot_token).",
    ),
    (
        "claim",
        "Manage work claims — acquire, heartbeat, release, done, or list. Single consolidated tool with `action` field (acquire|done|heartbeat|list|release).",
    ),
    (
        "review",
        "Review operations — submit findings, run consensus, list/clear/record rounds, check scores. Single consolidated tool with `action` field (clear|consensus|list|record|scores|submit).",
    ),
    (
        "tool",
        "Tool operations — search both agentflare's own first-party tools and downstream MCP servers' tools by task description, or execute a downstream one. Single consolidated tool with `action` field (search|execute).",
    ),
    (
        "memory",
        "Memory operations — compact, context, curate, handoff, recall, relate, or remember observations. Single consolidated tool with `action` field (compact|context|curate|handoff|recall|relate|remember).",
    ),
    (
        "docs",
        "On-demand third-party package/API documentation — search|get|list|refresh. Rust crates via docs.rs (default); npm via npmjs.org (ecosystem=\"npm\"); Python via PyPI (ecosystem=\"python\").",
    ),
    (
        "vent",
        "Vent friction when the TOOLING blocks you (not the task) — a wrong/missing tool, a fabricated assumption, an environment gap.",
    ),
    (
        "vent_file",
        "List or file agentflare's own tooling bugs (git shim, af-guard, item-tracker friction) as ONE batched GitHub issue on getappz/agentflare.",
    ),
    (
        "flare_git",
        "GitHub repo management via the flare_git module. action=pr_create|pr_list|pr_get|pr_status|pr_merge|pr_comment|pr_request_review|issue_create|issue_list|issue_get|issue_comment|issue_close|issue_label|release_list|release_get|release_latest|release_create|run_list|run_get|run_rerun|workflow_dispatch.",
    ),
    (
        "optimize",
        "Optimize layer — reversible-compression retrieval (CCR). action=retrieve returns the original for a registered id; action=list enumerates live entries.",
    ),
    (
        "item",
        "Manage work items in the repo's linked project. Single consolidated tool with `action` field (create|get|list|search|update|update_state|delete|claim|heartbeat|release|done|check_merge|cancel|add_label|remove_label|groom|standup|health).",
    ),
    (
        "comment",
        "Create, edit, delete, or list threaded comments on an item. Single consolidated tool with `action` field (create|edit|delete|list).",
    ),
    (
        "label",
        "Manage labels in the repo's linked project. The `action` field selects the operation (create|list|update|delete).",
    ),
    (
        "webhook",
        "Register, list, or delete webhooks on the repo's linked workspace. The `action` field selects the operation.",
    ),
    (
        "project",
        "Show the workspace/project this repo is currently linked to (auto-created/linked on first use). The `action` field selects the operation (only `info` for now).",
    ),
    (
        "asset",
        "Attach, get, list, or delete file assets on items/projects. Attach requires the file to exist in ~/.agentflare/staging/<filename> first.",
    ),
    (
        "search",
        "Unified search across 17 sources. type='store' (FTS store docs), 'memory' (brain.db observations), 'code' (leanctx), 'web' (rivalsearch), 'social', 'news', 'github', 'academic', 'datasets', 'websites', 'weather', 'financial', 'crypto', 'fx', 'indicators', 'youtube', 'bluesky'.",
    ),
];

/// Score assigned to every builtin-catalog hit. `gateway_registry`'s local
/// FTS5 `search()` scores via SQLite's `bm25()`, which is always negative
/// (lower/more-negative = better match — see that crate's
/// `REGISTRY_FALLBACK_SCORE` doc comment for the same ascending convention).
/// `0.0` is not a value `bm25()` ever produces for an actual match, so it's
/// guaranteed worse than every real local FTS hit, while still far smaller
/// than `gateway_registry::REGISTRY_FALLBACK_SCORE` (`f64::MAX`) — builtin
/// hits sort strictly between "real downstream-server match" and "public
/// registry fallback" if the merged list is ever re-sorted by score using
/// that same convention. An arbitrary small negative (e.g. `-1.0`) would
/// carry no such guarantee, since a real bm25 score can land anywhere in
/// negative space depending on match quality.
const BUILTIN_HIT_SCORE: f64 = 0.0;

/// Case-insensitive, whitespace-tokenized overlap scoring — the catalog is
/// tiny (~24 entries), so a real BM25 index would be pure overhead. Ties
/// keep `BUILTIN_TOOLS` declaration order (stable sort), which is fine at
/// this scale. `mode` mirrors `gateway_registry::MatchMode`'s contract for
/// the downstream FTS5 search this sits beside: `All` requires every query
/// term to appear, `Any` requires at least one -- without this, `mode="all"`
/// (the tool's own default) would still surface a builtin hit on a single
/// overlapping term, e.g. `item nonexistent` wrongly returning
/// `mcp__flare__item`.
pub(crate) fn search_builtin_tools(
    query: &str,
    limit: usize,
    mode: gateway_registry::MatchMode,
) -> Vec<gateway_registry::ToolHit> {
    let terms: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, &'static str, &'static str)> = BUILTIN_TOOLS
        .iter()
        .filter_map(|&(name, desc)| {
            let haystack = format!("{name} {desc}").to_lowercase();
            let matched = terms
                .iter()
                .filter(|t| haystack.contains(t.as_str()))
                .count();
            let hit = match mode {
                gateway_registry::MatchMode::All => matched == terms.len(),
                gateway_registry::MatchMode::Any => matched > 0,
            };
            hit.then_some((matched, name, desc))
        })
        .collect();
    scored.sort_by_key(|s| std::cmp::Reverse(s.0));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, name, desc)| gateway_registry::ToolHit {
            server: "agentflare".to_string(),
            tool: format!("mcp__flare__{name}"),
            description: desc.to_string(),
            input_schema: serde_json::Value::Null,
            score: BUILTIN_HIT_SCORE,
            source: gateway_registry::HitSource::Local,
            install_hint: None,
            remote_url: None,
        })
        .collect()
}

/// Fold builtin-catalog hits into an already-fetched local (downstream
/// gateway server) result set, up to `limit` total — same shape as
/// `gateway_registry::merge_registry_hits`, applied one tier earlier so
/// builtin hits are preferred over public-registry fallback noise.
pub(crate) fn merge_builtin_hits(
    mut local: Vec<gateway_registry::ToolHit>,
    limit: usize,
    builtin: Vec<gateway_registry::ToolHit>,
) -> Vec<gateway_registry::ToolHit> {
    let remaining = limit.saturating_sub(local.len());
    local.extend(builtin.into_iter().take(remaining));
    local
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_item_tool_by_bare_name() {
        let hits = search_builtin_tools("item", 5, gateway_registry::MatchMode::Any);
        assert!(hits.iter().any(|h| h.tool == "mcp__flare__item"));
    }

    #[test]
    fn finds_handoff_tool_by_bare_name() {
        let hits = search_builtin_tools("handoff", 5, gateway_registry::MatchMode::Any);
        assert!(hits.iter().any(|h| h.tool == "mcp__flare__handoff"));
    }

    #[test]
    fn respects_limit() {
        let hits = search_builtin_tools("action field", 3, gateway_registry::MatchMode::Any);
        assert!(hits.len() <= 3);
    }

    #[test]
    fn empty_query_returns_no_hits() {
        assert!(search_builtin_tools("   ", 5, gateway_registry::MatchMode::Any).is_empty());
    }

    #[test]
    fn unrelated_query_returns_no_hits() {
        let hits = search_builtin_tools("zzznonexistentzzz", 5, gateway_registry::MatchMode::Any);
        assert!(hits.is_empty());
    }

    #[test]
    fn all_mode_requires_every_term_any_mode_requires_one() {
        // Regression for the exact scenario CodeRabbit flagged: with the
        // tool's own default mode ("all"), a query mixing a real term with
        // a nonsense one must NOT surface a hit on the real term alone --
        // that's Any-mode behavior masquerading as All.
        let all_hits =
            search_builtin_tools("item nonexistentxyz", 5, gateway_registry::MatchMode::All);
        assert!(
            all_hits.is_empty(),
            "All mode must require every term to match"
        );

        let any_hits =
            search_builtin_tools("item nonexistentxyz", 5, gateway_registry::MatchMode::Any);
        assert!(
            any_hits.iter().any(|h| h.tool == "mcp__flare__item"),
            "Any mode still matches on the one overlapping term"
        );
    }

    #[test]
    fn merge_builtin_hits_fills_remaining_quota_only() {
        let local = vec![gateway_registry::ToolHit {
            server: "leanctx".to_string(),
            tool: "ctx_read".to_string(),
            description: "read".to_string(),
            input_schema: serde_json::Value::Null,
            score: -5.0,
            source: gateway_registry::HitSource::Local,
            install_hint: None,
            remote_url: None,
        }];
        let builtin =
            search_builtin_tools("item asset handoff", 5, gateway_registry::MatchMode::Any);
        let merged = merge_builtin_hits(local, 2, builtin);
        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged[0].server, "leanctx",
            "the original local hit stays first"
        );
        assert_eq!(
            merged[1].server, "agentflare",
            "builtin fills the one remaining slot"
        );
    }

    #[test]
    fn merge_builtin_hits_skips_builtin_when_local_already_meets_limit() {
        let local = vec![gateway_registry::ToolHit {
            server: "leanctx".to_string(),
            tool: "ctx_read".to_string(),
            description: "read".to_string(),
            input_schema: serde_json::Value::Null,
            score: -5.0,
            source: gateway_registry::HitSource::Local,
            install_hint: None,
            remote_url: None,
        }];
        let builtin = search_builtin_tools("item", 5, gateway_registry::MatchMode::Any);
        let merged = merge_builtin_hits(local.clone(), local.len(), builtin);
        assert_eq!(merged.len(), local.len());
        assert!(merged.iter().all(|h| h.server == "leanctx"));
    }
}
