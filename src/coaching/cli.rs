//! CLI-facing presentation for `agentflare coaching {list,apply,remove}`.

use super::rule::RuleTrigger;
use super::store::{self, MAX_RULES};

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
        println!("  {:<10} {}  (applied {})", r.id, r.title, r.applied_at);
        println!("    {}", r.body);
        println!("    {}", describe_trigger(r.trigger.as_ref()));
    }
}

pub fn cli_apply(
    id: &str,
    title: &str,
    body: &str,
    trigger_tools: Vec<String>,
    trigger_auto: bool,
) {
    let trigger = if trigger_tools.is_empty() && !trigger_auto {
        None
    } else {
        Some(RuleTrigger {
            tools: trigger_tools,
            auto_match: trigger_auto,
        })
    };
    match store::apply_rule(id, title, body, trigger) {
        Ok(rule) => println!("Applied coaching rule '{}': {}", rule.id, rule.title),
        Err(e) => {
            crate::ui::error(&format!("agentflare coaching apply: {e}"));
            std::process::exit(1);
        }
    }
}

pub fn cli_remove(id: &str) {
    match store::remove_rule(id) {
        Ok(()) => crate::ui::success(&format!("Removed coaching rule '{id}'.")),
        Err(e) => {
            crate::ui::error(&format!("agentflare coaching remove: {e}"));
            std::process::exit(1);
        }
    }
}
