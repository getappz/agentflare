use std::collections::HashMap;

use chrono::{Datelike, Timelike};

use crate::evidence::CoverageMeta;
use crate::model::{FileEvent, Session, TokenUsage, ToolCall};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Analytics {
    pub total_sessions: usize,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub by_source: HashMap<String, usize>,
    pub by_model: HashMap<String, usize>,
    pub by_project: HashMap<String, usize>,
    /// Bucketed by session `updated_at` date (not per-turn activity).
    pub by_day_updated: Vec<DailyBucket>,
    pub tool_freq: HashMap<String, u64>,
    pub file_freq: HashMap<String, usize>,
    /// Fraction of tool calls with a failed/error status; `None` when there are none.
    pub tool_failure_rate: Option<f64>,
    /// read / (input + read); `None` on a zero denominator.
    pub cache_read_share: Option<f64>,
    /// read / (input + read + write); `None` on a zero denominator.
    pub cache_prompt_efficiency: Option<f64>,
    pub coverage: CoverageMeta,
}

fn ratio(num: u64, den: u64) -> Option<f64> {
    (den > 0).then(|| num as f64 / den as f64)
}

/// Cache metrics from summed token counts: (read_share, prompt_efficiency).
pub fn cache_metrics(input: u64, read: u64, write: u64) -> (Option<f64>, Option<f64>) {
    (ratio(read, input + read), ratio(read, input + read + write))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DailyBucket {
    pub date: String,
    pub sessions: usize,
    pub tokens: TokenUsage,
    pub cost_usd: f64,
}

// DRY: shared aggregation for sessions
pub fn compute_analytics(sessions: &[Session]) -> Analytics {
    compute_analytics_with_tools(sessions, &[], &[])
}

// DRY: with tools + files (used by opencode/claude)
pub fn compute_analytics_with_tools(
    sessions: &[Session],
    tools: &[ToolCall],
    files: &[FileEvent],
) -> Analytics {
    let mut total_tokens = 0u64;
    let mut total_cost = 0.0;
    let mut by_source: HashMap<String, usize> = HashMap::new();
    let mut by_model: HashMap<String, usize> = HashMap::new();
    let mut by_project: HashMap<String, usize> = HashMap::new();
    let mut by_day: HashMap<String, DailyBucket> = HashMap::new();
    let (mut t_in, mut t_read, mut t_write) = (0u64, 0u64, 0u64);
    // An empty list reports the same bounds as `stats_window`: now, not the epoch.
    let now = chrono::Utc::now();
    let mut coverage = CoverageMeta {
        window_start: sessions.iter().map(|s| s.updated_at).min().unwrap_or(now),
        window_end: sessions.iter().map(|s| s.updated_at).max().unwrap_or(now),
        sessions_in_window: sessions.len(),
        eligible_sessions: sessions.len(),
        eligible_turns: sessions.iter().map(|s| s.turn_count as usize).sum(),
        // This API receives session summaries, not turn timestamps.
        sessions_missing_turn_timestamps: sessions.len(),
        ..Default::default()
    };

    for s in sessions {
        total_tokens += s.tokens.total();
        if let Some(c) = &s.cost {
            total_cost += c.total_usd;
        }
        *by_source.entry(s.source.as_str().to_string()).or_default() += 1;
        if let Some(m) = &s.model {
            *by_model.entry(m.clone()).or_default() += 1;
        }
        *by_project.entry(s.project.clone()).or_default() += 1;

        let date = s.updated_at.format("%Y-%m-%d").to_string();
        let bucket = by_day.entry(date.clone()).or_insert(DailyBucket {
            date,
            sessions: 0,
            tokens: TokenUsage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost_usd: 0.0,
        });
        bucket.sessions += 1;
        bucket.tokens.input += s.tokens.input;
        bucket.tokens.output += s.tokens.output;
        bucket.tokens.cache_read += s.tokens.cache_read;
        bucket.tokens.cache_write += s.tokens.cache_write;
        bucket.tokens.reasoning += s.tokens.reasoning;
        if let Some(c) = &s.cost {
            bucket.cost_usd += c.total_usd;
        }

        t_in += s.tokens.input;
        t_read += s.tokens.cache_read;
        t_write += s.tokens.cache_write;
        coverage.unpriced_sessions += s.cost.is_none() as usize;
    }

    let mut by_day_vec: Vec<DailyBucket> = by_day.into_values().collect();
    by_day_vec.sort_by(|a, b| a.date.cmp(&b.date));

    let (cache_read_share, cache_prompt_efficiency) = cache_metrics(t_in, t_read, t_write);
    let failed = tools
        .iter()
        .filter(|t| matches!(t.status.as_str(), "failed" | "error"))
        .count() as u64;

    Analytics {
        total_sessions: sessions.len(),
        total_tokens,
        total_cost_usd: total_cost,
        by_source,
        by_model,
        by_project,
        by_day_updated: by_day_vec,
        tool_freq: tool_frequency(tools),
        file_freq: file_activity(files),
        tool_failure_rate: ratio(failed, tools.len() as u64),
        cache_read_share,
        cache_prompt_efficiency,
        coverage,
    }
}

pub fn tool_frequency(tools: &[ToolCall]) -> HashMap<String, u64> {
    let mut m: HashMap<String, u64> = HashMap::new();
    for t in tools {
        *m.entry(t.name.clone()).or_default() += 1;
    }
    m
}

pub fn file_activity(files: &[FileEvent]) -> HashMap<String, usize> {
    let mut m: HashMap<String, usize> = HashMap::new();
    for f in files {
        *m.entry(f.path.clone()).or_default() += 1;
    }
    m
}

pub fn top_expensive(sessions: &[Session], n: usize) -> Vec<&Session> {
    let mut v: Vec<&Session> = sessions.iter().collect();
    v.sort_by(|a, b| {
        let ca = a.cost.as_ref().map(|c| c.total_usd).unwrap_or(0.0);
        let cb = b.cost.as_ref().map(|c| c.total_usd).unwrap_or(0.0);
        cb.partial_cmp(&ca).unwrap()
    });
    v.truncate(n);
    v
}

pub fn heatmap_by_weekday_hour(sessions: &[Session]) -> [[u32; 24]; 7] {
    let mut grid = [[0u32; 24]; 7];
    for s in sessions {
        let w = s.updated_at.weekday().num_days_from_sunday() as usize;
        let h = s.updated_at.hour() as usize;
        grid[w][h] += 1;
    }
    grid
}

pub fn top_files(files: &[FileEvent], n: usize) -> Vec<(String, usize)> {
    let mut freq = file_activity(files);
    let mut v: Vec<(String, usize)> = freq.drain().collect();
    v.sort_by_key(|a| std::cmp::Reverse(a.1));
    v.truncate(n);
    v
}

pub fn top_tools(tools: &[ToolCall], n: usize) -> Vec<(String, u64)> {
    let mut freq = tool_frequency(tools);
    let mut v: Vec<(String, u64)> = freq.drain().collect();
    v.sort_by_key(|a| std::cmp::Reverse(a.1));
    v.truncate(n);
    v
}
