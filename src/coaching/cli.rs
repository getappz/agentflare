//! CLI-facing presentation for `agentflare coaching {list,apply,remove,sync}`.

use super::rule::{RuleTier, RuleTrigger};
use super::store::{self, MAX_RULES};

const ALL_HOSTS: &[&str] = &[
    "claude-code",
    "opencode",
    "cursor",
    "codex",
    "windsurf",
    "vscode-copilot",
    "cline",
];

fn describe_trigger(trigger: Option<&RuleTrigger>) -> String {
    match trigger {
        None => "no trigger — always shown at SessionStart".to_string(),
        Some(t) => {
            let mut parts = Vec::new();
            if !t.tools.is_empty() {
                parts.push(format!("tool:{}", t.tools.join(",")));
            }
            if t.auto_match {
                parts.push("auto (BM25 relevance)".to_string());
            }
            format!("trigger: {}", parts.join("; "))
        }
    }
}

pub fn print_list() {
    let rules = store::list_rules();
    if rules.is_empty() {
        println!(
            "No coaching rules configured. Add one with `agentflare coaching apply <id> --title <title> --body <body>`."
        );
        return;
    }
    println!("agentflare coaching rules ({}/{MAX_RULES}):\n", rules.len());
    for r in &rules {
        if !r.sync.is_empty() {
            println!(
                "  {:<10} {}  ({}, applied {}, synced: {})",
                r.id,
                r.title,
                r.tier.as_str(),
                r.applied_at,
                r.sync.join(", ")
            );
        } else {
            println!(
                "  {:<10} {}  ({}, applied {}, no sync)",
                r.id,
                r.title,
                r.tier.as_str(),
                r.applied_at
            );
        }
        println!("    {}", r.body);
        println!("    {}", describe_trigger(r.trigger.as_ref()));
        if r.enforced {
            println!("    [MANDATORY]");
        }
    }
}

pub fn cli_apply(
    id: &str,
    title: &str,
    body: &str,
    trigger_tools: Vec<String>,
    trigger_auto: bool,
    tier: Option<RuleTier>,
    sync: Vec<String>,
) {
    let trigger = if trigger_tools.is_empty() && !trigger_auto {
        None
    } else {
        Some(RuleTrigger {
            tools: trigger_tools,
            auto_match: trigger_auto,
        })
    };
    let tier = tier.unwrap_or(RuleTier::Override);
    let sync_hosts = sync.clone();
    match store::apply_rule(id, title, body, trigger, tier, sync) {
        Ok(rule) => {
            println!("Applied coaching rule '{}': {}", rule.id, rule.title);
            for host in &sync_hosts {
                match crate::components::sync_now(host) {
                    Ok(msg) => println!("  synced to {host}: {msg}"),
                    Err(e) => crate::ui::error(&format!("  failed to sync to {host}: {e}")),
                }
            }
        }
        Err(e) => {
            crate::ui::error(&format!("agentflare coaching apply: {e}"));
            std::process::exit(1);
        }
    }
}

pub fn cli_enforce(id: &str, enforced: bool) {
    match store::set_enforced(id, enforced) {
        Ok(rule) => {
            let state = if enforced { "MANDATORY" } else { "advisory" };
            println!("Rule '{}' is now {state}.", rule.id);
        }
        Err(e) => {
            crate::ui::error(&format!("agentflare coaching enforce: {e}"));
            std::process::exit(1);
        }
    }
}

pub fn cli_remove(id: &str) {
    match store::remove_rule(id) {
        Ok(sync) => {
            for host in &sync {
                if let Err(e) = crate::components::unsync_host(id, host) {
                    crate::ui::error(&format!("failed to unsync rule '{id}' from {host}: {e}"));
                    std::process::exit(1);
                }
            }
            crate::ui::success(&format!("Removed coaching rule '{id}'."));
        }
        Err(e) => {
            crate::ui::error(&format!("agentflare coaching remove: {e}"));
            std::process::exit(1);
        }
    }
}

pub fn cli_sync(agent: Option<&str>) {
    let targets: Vec<&str> = match agent {
        Some(a) => vec![a],
        None => ALL_HOSTS.to_vec(),
    };
    for host in targets {
        match crate::components::sync_now(host) {
            Ok(msg) => println!("{host}: {msg}"),
            Err(e) => crate::ui::error(&format!("{host}: {e}")),
        }
    }
}
