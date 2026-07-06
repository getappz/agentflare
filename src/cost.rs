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

pub(crate) struct LineUsage {
    pub(crate) model: Option<String>,
    pub(crate) tokens: TokenUsage,
    pub(crate) message_id: Option<String>,
    pub(crate) request_id: Option<String>,
    pub(crate) date: Option<NaiveDate>,
}

/// Parse one JSONL line's cost-relevant fields. Matches the shape Claude Code
/// writes: `usage`/`model` nested under `message` (assistant lines) with a
/// top-level fallback, `requestId` + `message.id` for dedup (Claude Code
/// writes one line per content block — thinking/text/tool_use — each
/// carrying the full response's usage), and an RFC3339 `timestamp`.
pub(crate) fn parse_line(raw: &str) -> Option<LineUsage> {
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

pub(crate) fn find_session_files_under(dir: &Path) -> Vec<PathBuf> {
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
pub(crate) fn should_count_line(line: &LineUsage, seen: &mut HashSet<String>) -> bool {
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

pub(crate) fn add_tokens(entry: &mut TokenUsage, tokens: &TokenUsage) {
    entry.input_tokens += tokens.input_tokens;
    entry.output_tokens += tokens.output_tokens;
    entry.cache_read_tokens += tokens.cache_read_tokens;
    entry.cache_creation_tokens += tokens.cache_creation_tokens;
    entry.cache_creation_5m_tokens += tokens.cache_creation_5m_tokens;
    entry.cache_creation_1hr_tokens += tokens.cache_creation_1hr_tokens;
}

pub(crate) enum GroupBy {
    Model,
    Project,
}

#[derive(Default)]
pub(crate) struct GroupTotals {
    pub(crate) tokens: TokenUsage,
    pub(crate) cost_usd: f64,
    pub(crate) has_unpriced_usage: bool,
}

pub(crate) fn project_name_for(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Aggregates cost per JSONL line (i.e. per API call), using that line's own
/// parsed model and token counts, and sums the resulting `cost_usd` into each
/// output bucket (model or project). This is deliberate, not an oversight: it
/// must NOT be "refactored" into summing a bucket's tokens first and pricing
/// the aggregate once.
///
/// Anthropic's long-context pricing tier (see `long_context_pricing` in
/// `data/anthropic-pricing.json`, applied via `tiered_cost()` in
/// `src/pricing.rs`) is a per-request property: the 200k-token threshold
/// describes a single call's context size, not a cumulative total across many
/// separate calls in a day. Pricing each line individually — as done here —
/// correctly mirrors that semantics. A bucket-level "sum tokens first, then
/// price" approach would instead incorrectly apply the long-context surcharge
/// to a model with many modest-sized calls whose tokens merely happen to sum
/// past 200k over the course of a day, even though none of those calls
/// individually crossed the threshold.
///
/// Kept `pub(crate)` after the rollup migration so `rollup`'s tests can use
/// it as an independent, already-tested reference implementation to check
/// query() results against — not because `run()` still calls it.
pub(crate) fn aggregate(
    files: &[PathBuf],
    date_range: (NaiveDate, NaiveDate),
    group_by: GroupBy,
    pricing: &HashMap<String, crate::pricing::ModelPricing>,
) -> HashMap<String, GroupTotals> {
    let mut totals: HashMap<String, GroupTotals> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let (start, end) = date_range;

    for path in files {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let project = project_name_for(path);

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Some(parsed) = parse_line(line) else {
                continue;
            };
            let Some(date) = parsed.date else {
                continue;
            };
            if date < start || date > end {
                continue;
            }
            if !should_count_line(&parsed, &mut seen) {
                continue;
            }

            let key = match group_by {
                GroupBy::Model => parsed.model.clone().unwrap_or_else(|| "unknown".to_string()),
                GroupBy::Project => project.clone(),
            };
            let cost = calculate_cost(&parsed.tokens, parsed.model.as_deref(), pricing);

            let entry = totals.entry(key).or_default();
            add_tokens(&mut entry.tokens, &parsed.tokens);
            entry.cost_usd += cost.total_usd;
            entry.has_unpriced_usage |= cost.has_unpriced_usage;
        }
    }

    totals
}

pub fn run(days: Option<u32>, by_project: bool) {
    let today = Local::now().date_naive();
    let window = days.unwrap_or(1).max(1);
    let start = today - chrono::Duration::days(window as i64 - 1);
    let group_by = if by_project { GroupBy::Project } else { GroupBy::Model };

    let mut conn = crate::rollup::open_or_rebuild();
    crate::rollup::sync(&mut conn, &claude_projects_dir());
    let totals = crate::rollup::query(&conn, (start, today), group_by);

    let range_label = if window == 1 {
        format!("today ({today})")
    } else {
        format!("the last {window} days ({start}..{today})")
    };
    let group_label = if by_project { "project" } else { "model" };

    if totals.is_empty() {
        println!("No Claude Code sessions found for {range_label}.");
        return;
    }

    if window == 1 && !by_project {
        println!("agentflare cost — {today}\n");
    } else {
        println!("agentflare cost — {range_label}, by {group_label}\n");
    }

    let mut rows: Vec<_> = totals.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));

    // Model names always fit in 32 chars, so this floor keeps the no-flags/
    // by-model output byte-identical; project directory names can be much
    // longer, so the column widens to fit them instead of going ragged.
    let key_width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(32).max(32);

    let mut total_cost = 0.0;
    let mut total_tokens = TokenUsage::default();
    let mut any_unpriced = false;

    for (key, group) in &rows {
        total_cost += group.cost_usd;
        add_tokens(&mut total_tokens, &group.tokens);
        any_unpriced |= group.has_unpriced_usage;

        println!(
            "  {:<key_width$} in {:>9}  out {:>8}  cache-r {:>9}  cache-w {:>8}   ${:.4}",
            key,
            group.tokens.input_tokens,
            group.tokens.output_tokens,
            group.tokens.cache_read_tokens,
            group.tokens.cache_creation_tokens,
            group.cost_usd,
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
    fn aggregate_filters_by_calendar_date_and_sums_per_model() {
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

        let pricing = load_pricing();
        let totals = aggregate(&files, (today, today), GroupBy::Model, &pricing);
        let opus = totals.get("claude-opus-4-8").expect("expected opus entry");
        assert_eq!(opus.tokens.input_tokens, 120, "yesterday's tokens must be excluded when range is today-only");
        assert_eq!(opus.tokens.output_tokens, 60);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn aggregate_with_wider_date_range_includes_prior_days_but_not_older() {
        let dir = std::env::temp_dir().join("agentflare-test-cost-aggregate-range");
        let _ = std::fs::remove_dir_all(&dir);
        let project_dir = dir.join("proj1");
        std::fs::create_dir_all(&project_dir).unwrap();

        let today = NaiveDate::from_ymd_opt(2026, 7, 6).unwrap();

        let content = format!(
            "{}\n{}\n",
            r#"{"type":"assistant","timestamp":"2026-07-04T10:00:00Z","message":{"id":"m1","model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":50}},"requestId":"r1"}"#,
            r#"{"type":"assistant","timestamp":"2026-07-03T10:00:00Z","message":{"id":"m2","model":"claude-opus-4-8","usage":{"input_tokens":999,"output_tokens":999}},"requestId":"r2"}"#,
        );
        std::fs::write(project_dir.join("session1.jsonl"), content).unwrap();

        let files = find_session_files_under(&dir);

        // 3-day window ending today: 2026-07-04, 2026-07-05, 2026-07-06.
        // Includes the 07-04 line, excludes the 07-03 line.
        let range = (today - chrono::Duration::days(2), today);

        let pricing = load_pricing();
        let totals = aggregate(&files, range, GroupBy::Model, &pricing);
        let opus = totals.get("claude-opus-4-8").expect("expected opus entry from within range");
        assert_eq!(opus.tokens.input_tokens, 100, "the 2026-07-03 line is outside the 3-day window and must be excluded");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn aggregate_by_project_groups_by_parent_directory_name() {
        let dir = std::env::temp_dir().join("agentflare-test-cost-aggregate-project");
        let _ = std::fs::remove_dir_all(&dir);
        let project_a = dir.join("proj-a");
        let project_b = dir.join("proj-b");
        std::fs::create_dir_all(&project_a).unwrap();
        std::fs::create_dir_all(&project_b).unwrap();

        let today = NaiveDate::from_ymd_opt(2026, 7, 6).unwrap();
        let line_a = r#"{"type":"assistant","timestamp":"2026-07-06T10:00:00Z","message":{"id":"ma","model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":50}},"requestId":"ra"}"#;
        let line_b = r#"{"type":"assistant","timestamp":"2026-07-06T10:00:00Z","message":{"id":"mb","model":"claude-sonnet-5","usage":{"input_tokens":10,"output_tokens":5}},"requestId":"rb"}"#;
        std::fs::write(project_a.join("session1.jsonl"), format!("{line_a}\n")).unwrap();
        std::fs::write(project_b.join("session1.jsonl"), format!("{line_b}\n")).unwrap();

        let files = find_session_files_under(&dir);
        assert_eq!(files.len(), 2);

        let pricing = load_pricing();
        let totals = aggregate(&files, (today, today), GroupBy::Project, &pricing);
        assert_eq!(totals.get("proj-a").expect("expected proj-a entry").tokens.input_tokens, 100);
        assert_eq!(totals.get("proj-b").expect("expected proj-b entry").tokens.input_tokens, 10);
        assert!(totals.get("claude-opus-4-8").is_none(), "grouping by project must not key by model name");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn aggregate_by_project_prices_each_line_by_its_own_model() {
        // A project bucket mixing two models at different rates must sum
        // per-line cost, not price the whole bucket once with one model.
        let dir = std::env::temp_dir().join("agentflare-test-cost-aggregate-project-pricing");
        let _ = std::fs::remove_dir_all(&dir);
        let project_dir = dir.join("proj-mixed");
        std::fs::create_dir_all(&project_dir).unwrap();

        let opus_line = r#"{"type":"assistant","timestamp":"2026-07-06T10:00:00Z","message":{"id":"m1","model":"claude-opus-4-8","usage":{"input_tokens":1000,"output_tokens":0}},"requestId":"r1"}"#;
        let haiku_line = r#"{"type":"assistant","timestamp":"2026-07-06T10:00:00Z","message":{"id":"m2","model":"claude-haiku-4-5","usage":{"input_tokens":1000,"output_tokens":0}},"requestId":"r2"}"#;
        std::fs::write(project_dir.join("session1.jsonl"), format!("{opus_line}\n{haiku_line}\n")).unwrap();

        let files = find_session_files_under(&dir);
        let today = NaiveDate::from_ymd_opt(2026, 7, 6).unwrap();
        let pricing = load_pricing();

        let by_project = aggregate(&files, (today, today), GroupBy::Project, &pricing);
        let project_cost = by_project.get("proj-mixed").expect("expected proj-mixed entry").cost_usd;

        let by_model = aggregate(&files, (today, today), GroupBy::Model, &pricing);
        let expected_total: f64 = by_model.values().map(|g| g.cost_usd).sum();

        assert!(
            (project_cost - expected_total).abs() < 0.000_001,
            "project-grouped cost ({project_cost}) must equal the sum of correctly-priced per-model costs ({expected_total}), not a single blended/unpriced rate"
        );
        assert!(project_cost > 0.0, "opus and haiku are both known, priced models — cost must be nonzero");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn aggregate_prices_each_call_by_its_own_context_size_not_the_daily_sum() {
        // claude-sonnet-4-5-20250929 has long_context_pricing active: a 200,000
        // input-token threshold, $3/mtok below it, $6/mtok above. Two calls of
        // 150,000 input tokens each stay under the threshold individually, but
        // their sum (300,000) crosses it. If aggregate() summed tokens first
        // and priced the bucket once, the surcharge would apply to the top
        // 100,000 tokens; pricing each line by its own call correctly avoids
        // that, since neither call ever saw more than 150,000 tokens of context.
        const MODEL: &str = "claude-sonnet-4-5-20250929";
        let dir = std::env::temp_dir().join("agentflare-test-cost-aggregate-tiered-per-line");
        let _ = std::fs::remove_dir_all(&dir);
        let project_dir = dir.join("proj-tiered");
        std::fs::create_dir_all(&project_dir).unwrap();

        let line_1 = format!(
            r#"{{"type":"assistant","timestamp":"2026-07-06T10:00:00Z","message":{{"id":"m1","model":"{MODEL}","usage":{{"input_tokens":150000,"output_tokens":0}}}},"requestId":"r1"}}"#
        );
        let line_2 = format!(
            r#"{{"type":"assistant","timestamp":"2026-07-06T10:00:00Z","message":{{"id":"m2","model":"{MODEL}","usage":{{"input_tokens":150000,"output_tokens":0}}}},"requestId":"r2"}}"#
        );
        std::fs::write(project_dir.join("session1.jsonl"), format!("{line_1}\n{line_2}\n")).unwrap();

        let files = find_session_files_under(&dir);
        let today = NaiveDate::from_ymd_opt(2026, 7, 6).unwrap();
        let pricing = load_pricing();

        let by_model = aggregate(&files, (today, today), GroupBy::Model, &pricing);
        let actual_cost = by_model.get(MODEL).expect("expected model entry").cost_usd;

        // Hypothetical: what the cost WOULD be if both lines' tokens were
        // summed first and priced once as a single 300,000-token call.
        let combined_tokens = crate::pricing::TokenUsage { input_tokens: 300_000, ..Default::default() };
        let combined_cost =
            crate::pricing::calculate_cost(&combined_tokens, Some(MODEL), &pricing).total_usd;

        assert!(
            actual_cost < combined_cost,
            "per-line pricing ({actual_cost}) must be cheaper than pricing the summed \
             daily total once ({combined_cost}) — otherwise the long-context surcharge is \
             being applied to a cumulative total instead of each call's own context size"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
