//! `agentflare decide dataset stats`: per-site volume, label balance, date
//! range and how much the normalized inputs repeat (where a deterministic
//! engine can pay off).
use super::Row;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

const TOP_K: usize = 5;

#[derive(Debug, Default, PartialEq)]
pub struct SiteStats {
    pub site: String,
    pub rows: usize,
    pub labels: BTreeMap<String, usize>,
    pub first_ts: i64,
    pub last_ts: i64,
    /// Share of rows whose `norm_key` already appeared earlier.
    pub repetition: f64,
    /// Share of rows covered by the `TOP_K` most frequent keys.
    pub top_k_coverage: f64,
    pub distinct_keys: usize,
}

pub fn summarize(rows: &[Row]) -> Vec<SiteStats> {
    let mut by_site: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for r in rows {
        by_site.entry(&r.site).or_default().push(r);
    }
    by_site
        .into_iter()
        .map(|(site, rs)| {
            let mut s = SiteStats {
                site: site.to_string(),
                rows: rs.len(),
                first_ts: i64::MAX,
                ..Default::default()
            };
            let mut keys: HashMap<&str, usize> = HashMap::new();
            let mut repeats = 0usize;
            for r in &rs {
                s.first_ts = s.first_ts.min(r.ts);
                s.last_ts = s.last_ts.max(r.ts);
                let label = r.label["summary"].as_str().unwrap_or("?");
                *s.labels.entry(label.to_string()).or_default() += 1;
                let n = keys.entry(&r.norm_key).or_default();
                if *n > 0 {
                    repeats += 1;
                }
                *n += 1;
            }
            let mut counts: Vec<usize> = keys.values().copied().collect();
            counts.sort_unstable_by(|a, b| b.cmp(a));
            let total = rs.len() as f64;
            s.distinct_keys = counts.len();
            s.repetition = repeats as f64 / total;
            s.top_k_coverage = counts.iter().take(TOP_K).sum::<usize>() as f64 / total;
            s
        })
        .collect()
}

fn day(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0).map_or("?".into(), |d| d.format("%Y-%m-%d").to_string())
}

pub fn render(stats: &[SiteStats]) -> String {
    if stats.is_empty() {
        return "No captured rows. Set AGENTFLARE_DECIDE_CAPTURE=1 (plus a Jev mode) to collect.\n"
            .to_string();
    }
    let mut out = String::new();
    for s in stats {
        let _ = writeln!(
            out,
            "{}: {} rows, {} .. {}",
            s.site,
            s.rows,
            day(s.first_ts),
            day(s.last_ts)
        );
        let labels: Vec<String> = s.labels.iter().map(|(k, n)| format!("{k}={n}")).collect();
        let _ = writeln!(out, "  labels: {}", labels.join(", "));
        let _ = writeln!(
            out,
            "  repetition: {:.1}% ({} distinct keys), top-{TOP_K} coverage: {:.1}%",
            s.repetition * 100.0,
            s.distinct_keys,
            s.top_k_coverage * 100.0
        );
    }
    out
}
