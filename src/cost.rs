// `agentflare cost` — reads today's Claude Code session transcripts and
// prints a token/cost summary. Session discovery + JSONL field extraction is
// a minimal re-implementation of what claude-view's much larger accumulator
// does; the pricing math it calls into (src/pricing.rs) is ported directly.
// See /NOTICE.
use crate::pricing::{calculate_cost, load_pricing, TokenUsage};
use chrono::{DateTime, Local, NaiveDate};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

fn claude_projects_dir() -> PathBuf {
    crate::paths::home().join(".claude").join("projects")
}

struct LineUsage {
    model: Option<String>,
    tokens: TokenUsage,
    message_id: Option<String>,
    request_id: Option<String>,
    date: Option<NaiveDate>,
}

/// Parse one JSONL line's cost-relevant fields. Matches the shape Claude Code
/// writes: `usage`/`model` nested under `message` (assistant lines) with a
/// top-level fallback, `requestId` + `message.id` for dedup (Claude Code
/// writes one line per content block — thinking/text/tool_use — each
/// carrying the full response's usage), and an RFC3339 `timestamp`.
fn parse_line(raw: &str) -> Option<LineUsage> {
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    let msg = parsed.get("message");

    let model = parsed
        .get("model")
        .or_else(|| msg.and_then(|m| m.get("model")))
        .and_then(|v| v.as_str())
        .map(String::from);

    let usage = parsed
        .get("usage")
        .or_else(|| msg.and_then(|m| m.get("usage")));
    let tokens = usage
        .map(|u| TokenUsage {
            input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            cache_read_tokens: u
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_creation_tokens: u
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_creation_5m_tokens: u
                .get("cache_creation")
                .and_then(|cc| cc.get("ephemeral_5m_input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_creation_1hr_tokens: u
                .get("cache_creation")
                .and_then(|cc| cc.get("ephemeral_1h_input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        })
        .unwrap_or_default();

    let message_id = msg
        .and_then(|m| m.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let request_id = parsed
        .get("requestId")
        .and_then(|v| v.as_str())
        .map(String::from);

    let date = parsed
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|dt| dt.with_timezone(&Local).date_naive());

    Some(LineUsage {
        model,
        tokens,
        message_id,
        request_id,
        date,
    })
}

fn find_session_files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = vec![];
    let Ok(project_entries) = std::fs::read_dir(dir) else {
        return files;
    };
    for project in project_entries.flatten() {
        let path = project.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(session_entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for session in session_entries.flatten() {
            let p = session.path();
            if p.extension().map(|e| e == "jsonl").unwrap_or(false) {
                files.push(p);
            }
        }
    }
    files
}

/// Whether a line's tokens should be counted, applying the same
/// content-block dedup Claude Code's own JSONL format needs: one API
/// response can appear as multiple lines (one per content block), each
/// carrying the full usage — count it once via `message.id:requestId`.
fn should_count_line(line: &LineUsage, seen: &mut HashSet<String>) -> bool {
    let has_measurement = line.tokens.input_tokens > 0
        || line.tokens.output_tokens > 0
        || line.tokens.cache_read_tokens > 0
        || line.tokens.cache_creation_tokens > 0;

    match (&line.message_id, &line.request_id) {
        (Some(mid), Some(rid)) => {
            if has_measurement {
                seen.insert(format!("{mid}:{rid}"))
            } else {
                false
            }
        }
        _ => has_measurement,
    }
}

fn add_tokens(entry: &mut TokenUsage, tokens: &TokenUsage) {
    entry.input_tokens += tokens.input_tokens;
    entry.output_tokens += tokens.output_tokens;
    entry.cache_read_tokens += tokens.cache_read_tokens;
    entry.cache_creation_tokens += tokens.cache_creation_tokens;
    entry.cache_creation_5m_tokens += tokens.cache_creation_5m_tokens;
    entry.cache_creation_1hr_tokens += tokens.cache_creation_1hr_tokens;
}

fn aggregate_today(files: &[PathBuf], today: NaiveDate) -> HashMap<String, TokenUsage> {
    let mut by_model: HashMap<String, TokenUsage> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();

    for path in files {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Some(parsed) = parse_line(line) else {
                continue;
            };
            if parsed.date != Some(today) {
                continue;
            }
            if !should_count_line(&parsed, &mut seen) {
                continue;
            }
            let model = parsed.model.unwrap_or_else(|| "unknown".to_string());
            add_tokens(by_model.entry(model).or_default(), &parsed.tokens);
        }
    }

    by_model
}

pub fn run() {
    let today = Local::now().date_naive();
    let files = find_session_files_under(&claude_projects_dir());
    let by_model = aggregate_today(&files, today);
    let pricing = load_pricing();

    if by_model.is_empty() {
        println!("No Claude Code sessions found for today ({today}).");
        return;
    }

    println!("agentflare cost — {today}\n");

    let mut models: Vec<_> = by_model.iter().collect();
    models.sort_by(|a, b| a.0.cmp(b.0));

    let mut total_cost = 0.0;
    let mut total_tokens = TokenUsage::default();
    let mut any_unpriced = false;

    for (model, tokens) in &models {
        let cost = calculate_cost(tokens, Some(model), &pricing);
        total_cost += cost.total_usd;
        add_tokens(&mut total_tokens, tokens);
        any_unpriced |= cost.has_unpriced_usage;

        println!(
            "  {:<32} in {:>9}  out {:>8}  cache-r {:>9}  cache-w {:>8}   ${:.4}",
            model,
            tokens.input_tokens,
            tokens.output_tokens,
            tokens.cache_read_tokens,
            tokens.cache_creation_tokens,
            cost.total_usd,
        );
    }

    println!();
    println!(
        "Total: {} in / {} out / {} cache-read / {} cache-write tokens — ${:.4}",
        total_tokens.input_tokens,
        total_tokens.output_tokens,
        total_tokens.cache_read_tokens,
        total_tokens.cache_creation_tokens,
        total_cost,
    );
    if any_unpriced {
        println!("(usage from unrecognized models is excluded from the cost total)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_line_reads_nested_message_fields() {
        let raw = r#"{"type":"assistant","timestamp":"2026-07-06T12:00:00Z","message":{"id":"msg_1","model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":10,"cache_creation_input_tokens":5}},"requestId":"req_1"}"#;
        let line = parse_line(raw).unwrap();
        assert_eq!(line.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(line.tokens.input_tokens, 100);
        assert_eq!(line.tokens.output_tokens, 50);
        assert_eq!(line.tokens.cache_read_tokens, 10);
        assert_eq!(line.tokens.cache_creation_tokens, 5);
        assert_eq!(line.message_id.as_deref(), Some("msg_1"));
        assert_eq!(line.request_id.as_deref(), Some("req_1"));
    }

    #[test]
    fn parse_line_reads_ephemeral_cache_split() {
        let raw = r#"{"type":"assistant","message":{"usage":{"cache_creation":{"ephemeral_5m_input_tokens":7,"ephemeral_1h_input_tokens":3}}}}"#;
        let line = parse_line(raw).unwrap();
        assert_eq!(line.tokens.cache_creation_5m_tokens, 7);
        assert_eq!(line.tokens.cache_creation_1hr_tokens, 3);
    }

    #[test]
    fn parse_line_returns_none_on_invalid_json() {
        assert!(parse_line("not json").is_none());
    }

    #[test]
    fn should_count_line_dedups_by_message_and_request_id() {
        let mut seen = HashSet::new();
        let make = |input: u64| LineUsage {
            model: None,
            tokens: TokenUsage { input_tokens: input, ..Default::default() },
            message_id: Some("msg_1".to_string()),
            request_id: Some("req_1".to_string()),
            date: None,
        };
        assert!(should_count_line(&make(10), &mut seen));
        // Same message_id:request_id pair, different content block — must not double-count.
        assert!(!should_count_line(&make(10), &mut seen));
    }

    #[test]
    fn should_count_line_counts_lines_without_ids_when_measured() {
        let mut seen = HashSet::new();
        let line = LineUsage {
            model: None,
            tokens: TokenUsage { input_tokens: 5, ..Default::default() },
            message_id: None,
            request_id: None,
            date: None,
        };
        assert!(should_count_line(&line, &mut seen));
    }

    #[test]
    fn should_count_line_skips_zero_measurement_blocks() {
        let mut seen = HashSet::new();
        let line = LineUsage {
            model: None,
            tokens: TokenUsage::default(),
            message_id: Some("msg_1".to_string()),
            request_id: Some("req_1".to_string()),
            date: None,
        };
        assert!(!should_count_line(&line, &mut seen));
    }

    #[test]
    fn aggregate_today_filters_by_calendar_date_and_sums_per_model() {
        let dir = std::env::temp_dir().join("agentflare-test-cost-aggregate");
        let _ = std::fs::remove_dir_all(&dir);
        let project_dir = dir.join("proj1");
        std::fs::create_dir_all(&project_dir).unwrap();

        let today = NaiveDate::from_ymd_opt(2026, 7, 6).unwrap();
        let today_ts = "2026-07-06T10:00:00Z";
        let yesterday_ts = "2026-07-05T10:00:00Z";

        let content = format!(
            "{}\n{}\n{}\n",
            format!(
                r#"{{"type":"assistant","timestamp":"{today_ts}","message":{{"id":"m1","model":"claude-opus-4-8","usage":{{"input_tokens":100,"output_tokens":50}}}},"requestId":"r1"}}"#
            ),
            format!(
                r#"{{"type":"assistant","timestamp":"{today_ts}","message":{{"id":"m2","model":"claude-opus-4-8","usage":{{"input_tokens":20,"output_tokens":10}}}},"requestId":"r2"}}"#
            ),
            format!(
                r#"{{"type":"assistant","timestamp":"{yesterday_ts}","message":{{"id":"m3","model":"claude-opus-4-8","usage":{{"input_tokens":999,"output_tokens":999}}}},"requestId":"r3"}}"#
            ),
        );
        std::fs::write(project_dir.join("session1.jsonl"), content).unwrap();

        let files = find_session_files_under(&dir);
        assert_eq!(files.len(), 1);

        let by_model = aggregate_today(&files, today);
        let opus = by_model.get("claude-opus-4-8").expect("expected opus entry");
        assert_eq!(opus.input_tokens, 120, "yesterday's tokens must be excluded");
        assert_eq!(opus.output_tokens, 60);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
