use std::collections::HashMap;

use chrono::{Datelike, Timelike};

use crate::model::{FileEvent, Session, TokenUsage, ToolCall};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Analytics {
    pub total_sessions: usize,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub by_source: HashMap<String, usize>,
    pub by_model: HashMap<String, usize>,
    pub by_project: HashMap<String, usize>,
    pub by_day: Vec<DailyBucket>,
    pub tool_freq: HashMap<String, u64>,
    pub file_freq: HashMap<String, usize>,
    pub cache_hit_rate: f64,
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
    let mut cache_read = 0u64;
    let mut cache_total = 0u64;

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

        cache_read += s.tokens.cache_read;
        cache_total += s.tokens.cache_read + s.tokens.input;
    }

    let mut by_day_vec: Vec<DailyBucket> = by_day.into_values().collect();
    by_day_vec.sort_by(|a, b| a.date.cmp(&b.date));

    let cache_hit_rate = if cache_total == 0 {
        0.0
    } else {
        cache_read as f64 / cache_total as f64
    };

    Analytics {
        total_sessions: sessions.len(),
        total_tokens,
        total_cost_usd: total_cost,
        by_source,
        by_model,
        by_project,
        by_day: by_day_vec,
        tool_freq: tool_frequency(tools),
        file_freq: file_activity(files),
        cache_hit_rate,
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
