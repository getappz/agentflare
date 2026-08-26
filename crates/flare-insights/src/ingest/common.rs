//! DRY helpers shared by Claude/Codex/OpenCode adapters.
//! Adopted from .refs/agent-trail, .refs/agent-eval/src/rollout/readers/opencode-sqlite.ts

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::model::{Cost, TokenUsage};

/// Try multiple timestamp formats: RFC3339, epoch ms, epoch sec.
pub fn parse_timestamp(v: &Value) -> Option<DateTime<Utc>> {
    if let Some(s) = v.as_str() {
        if let Ok(dt) = s.parse::<DateTime<Utc>>() {
            return Some(dt);
        }
        if let Ok(ms) = s.parse::<i64>() {
            return DateTime::from_timestamp_millis(ms);
        }
    }
    if let Some(n) = v.as_i64() {
        // heuristic: >1e12 = ms, else sec
        if n > 1_000_000_000_000 {
            return DateTime::from_timestamp_millis(n);
        }
        return DateTime::from_timestamp(n, 0);
    }
    if let Some(n) = v.as_u64() {
        let n = n as i64;
        if n > 1_000_000_000_000 {
            return DateTime::from_timestamp_millis(n);
        }
        return DateTime::from_timestamp(n, 0);
    }
    None
}

pub fn parse_timestamp_any(v: Option<&Value>) -> Option<DateTime<Utc>> {
    v.and_then(parse_timestamp)
}

/// Extract TokenUsage from various shapes:
/// Claude: {input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens}
/// OpenCode: {input, output, reasoning, cache:{read,write}} or {total,input,output}
pub fn extract_tokens(v: &Value) -> TokenUsage {
    let mut t = TokenUsage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        reasoning: 0,
    };
    if !v.is_object() {
        return t;
    }
    t.input = v
        .get("input_tokens")
        .or_else(|| v.get("input"))
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    t.output = v
        .get("output_tokens")
        .or_else(|| v.get("output"))
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    t.cache_read = v
        .get("cache_read_input_tokens")
        .or_else(|| v.get("cache").and_then(|c| c.get("read")))
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    t.cache_write = v
        .get("cache_creation_input_tokens")
        .or_else(|| v.get("cache").and_then(|c| c.get("write")))
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    t.reasoning = v
        .get("reasoning_tokens")
        .or_else(|| v.get("reasoning"))
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    // fallback: total -> split?
    if t.input == 0 && t.output == 0 {
        if let Some(total) = v.get("total").and_then(|n| n.as_u64()) {
            t.input = total;
        }
    }
    t
}

pub fn extract_cost(v: &Value) -> Option<Cost> {
    let total = v
        .get("cost_usd")
        .or_else(|| v.get("cost"))
        .and_then(|n| n.as_f64())?;
    Some(Cost {
        total_usd: total,
        input_usd: 0.0,
        output_usd: 0.0,
        cache_read_usd: 0.0,
        cache_write_usd: 0.0,
    })
}

pub fn title_from_text(s: &str, n: usize) -> String {
    let mut t = s.chars().take(n).collect::<String>();
    t = t.replace('\n', " ").trim().to_string();
    if t.len() > n {
        t.truncate(n);
        t.push('…');
    }
    t
}

/// DRY: get nested JSON string `a.b.c`
pub fn get_nested_str<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for p in path {
        cur = cur.get(*p)?;
    }
    cur.as_str()
}
