//! Shadow log: for each decision site, record what the current code decided
//! (`baseline`) next to what Jev said, so `agentflare decide report` can show
//! the agreement rate before any site is switched over.
//!
//! Privacy: only a short hash and the length of the input are stored, never
//! the text. Writing is best-effort and must never affect behavior.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub ts: i64,
    pub site: String,
    pub input_sha: String,
    pub input_len: usize,
    pub baseline: String,
    #[serde(default)]
    pub jev: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub cost: Option<f64>,
    /// Set when Jev could not answer (timeout, 402, no key, ...): a fallback.
    #[serde(default)]
    pub error: Option<String>,
}

impl Row {
    fn base(site: &str, input: &str, baseline: &str) -> Self {
        Self {
            ts: chrono::Utc::now().timestamp(),
            site: site.to_string(),
            input_sha: hex::encode(&Sha256::digest(input.as_bytes())[..6]),
            input_len: input.len(),
            baseline: baseline.to_string(),
            jev: None,
            confidence: None,
            latency_ms: None,
            cost: None,
            error: None,
        }
    }

    pub fn answered(
        site: &str,
        input: &str,
        baseline: &str,
        jev: &str,
        confidence: Option<f64>,
        latency_ms: u64,
        cost: Option<f64>,
    ) -> Self {
        Self {
            jev: Some(jev.to_string()),
            confidence,
            latency_ms: Some(latency_ms),
            cost,
            ..Self::base(site, input, baseline)
        }
    }

    pub fn failed(site: &str, input: &str, baseline: &str, error: &str) -> Self {
        Self {
            error: Some(error.chars().take(80).collect()),
            ..Self::base(site, input, baseline)
        }
    }
}

pub fn log_path() -> PathBuf {
    crate::paths::agentflare_dir()
        .join("decide")
        .join("shadow.jsonl")
}

/// Best-effort append to the default log; errors are deliberately ignored.
pub fn record(row: &Row) {
    let _ = append(&log_path(), row);
}

fn append(path: &Path, row: &Row) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let old = path.with_extension("jsonl.1");
        let _ = std::fs::remove_file(&old); // rename won't overwrite on Windows
        std::fs::rename(path, old)?;
    }
    let mut line = serde_json::to_string(row).map_err(std::io::Error::other)?;
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

/// Read every parseable row; a missing file or a damaged line is skipped.
pub fn load(path: &Path) -> Vec<Row> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[derive(Debug, Default, PartialEq)]
pub struct SiteStats {
    pub site: String,
    pub total: usize,
    pub errors: usize,
    pub answered: usize,
    pub agree: usize,
    /// (agreeing, answered) among rows with confidence >= the threshold.
    pub conf80: (usize, usize),
    pub conf90: (usize, usize),
    pub median_ms: Option<u64>,
    pub cost: f64,
}

pub fn summarize(rows: &[Row], only_site: Option<&str>) -> Vec<SiteStats> {
    let mut by_site: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for r in rows
        .iter()
        .filter(|r| only_site.is_none_or(|s| s == r.site))
    {
        by_site.entry(&r.site).or_default().push(r);
    }
    by_site
        .into_iter()
        .map(|(site, rs)| {
            let mut s = SiteStats {
                site: site.to_string(),
                total: rs.len(),
                ..SiteStats::default()
            };
            let mut latencies = Vec::new();
            for r in &rs {
                s.cost += r.cost.unwrap_or(0.0);
                let Some(jev) = r.jev.as_ref().filter(|_| r.error.is_none()) else {
                    s.errors += 1;
                    continue;
                };
                s.answered += 1;
                let agrees = *jev == r.baseline;
                s.agree += usize::from(agrees);
                for (bucket, min) in [(&mut s.conf80, 0.8), (&mut s.conf90, 0.9)] {
                    if r.confidence.is_some_and(|c| c >= min) {
                        bucket.1 += 1;
                        bucket.0 += usize::from(agrees);
                    }
                }
                latencies.extend(r.latency_ms);
            }
            latencies.sort_unstable();
            s.median_ms = latencies.get(latencies.len() / 2).copied();
            s
        })
        .collect()
}

fn pct(part: usize, whole: usize) -> String {
    if whole == 0 {
        "n/a".to_string()
    } else {
        format!(
            "{:.0}% ({part}/{whole})",
            100.0 * part as f64 / whole as f64
        )
    }
}

pub fn render(stats: &[SiteStats]) -> String {
    if stats.is_empty() {
        return "no shadow data yet\n".to_string();
    }
    let mut out = String::new();
    for s in stats {
        out.push_str(&format!(
            "{}\n  decisions      {} ({} fell back: {})\n  agreement      {}\n  at conf >= .8  {}\n  at conf >= .9  {}\n  median latency {}\n  total cost     ${:.6}\n",
            s.site,
            s.total,
            s.errors,
            pct(s.errors, s.total),
            pct(s.agree, s.answered),
            pct(s.conf80.0, s.conf80.1),
            pct(s.conf90.0, s.conf90.1),
            s.median_ms.map_or("n/a".to_string(), |m| format!("{m} ms")),
            s.cost,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(site: &str, baseline: &str, jev: &str, conf: Option<f64>, ms: u64) -> Row {
        Row::answered(site, "some input", baseline, jev, conf, ms, Some(0.00001))
    }

    #[test]
    fn append_then_load_round_trips_and_stores_no_input_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d").join("shadow.jsonl");
        append(&path, &ok("router", "haiku", "haiku", Some(0.9), 700)).unwrap();
        append(
            &path,
            &Row::failed("router", "secret prompt text", "haiku", "HTTP 402"),
        )
        .unwrap();
        let rows = load(&path);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].error.as_deref(), Some("HTTP 402"));
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("secret prompt text") && !raw.contains("some input"));
        assert_eq!(rows[1].input_len, "secret prompt text".len());
    }

    #[test]
    fn damaged_and_blank_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shadow.jsonl");
        append(&path, &ok("router", "a", "a", None, 1)).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{not json\n\n")
            .unwrap();
        append(&path, &ok("router", "a", "b", None, 1)).unwrap();
        assert_eq!(load(&path).len(), 2);
        assert!(load(&dir.path().join("missing.jsonl")).is_empty());
    }

    #[test]
    fn oversized_log_rotates_to_one_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shadow.jsonl");
        std::fs::write(&path, vec![b'x'; (MAX_BYTES + 1) as usize]).unwrap();
        append(&path, &ok("router", "a", "a", None, 1)).unwrap();
        assert_eq!(load(&path).len(), 1); // fresh file
        assert!(path.with_extension("jsonl.1").exists());
        // a second rotation replaces the backup instead of failing
        std::fs::write(&path, vec![b'y'; (MAX_BYTES + 1) as usize]).unwrap();
        append(&path, &ok("router", "a", "a", None, 1)).unwrap();
        assert_eq!(load(&path).len(), 1);
    }

    #[test]
    fn summarize_computes_agreement_confidence_buckets_latency_cost_and_fallbacks() {
        let rows = vec![
            ok("router", "haiku", "haiku", Some(0.95), 600),
            ok("router", "haiku", "opus", Some(0.85), 800),
            ok("router", "sonnet", "sonnet", Some(0.5), 700),
            Row::failed("router", "x", "haiku", "timeout"),
            ok("sdd_judge", "fix_round", "fix_round", Some(1.0), 900),
        ];
        let all = summarize(&rows, None);
        assert_eq!(all.len(), 2);
        let r = &all[0];
        assert_eq!(r.site, "router");
        assert_eq!((r.total, r.errors, r.answered, r.agree), (4, 1, 3, 2));
        assert_eq!(r.conf80, (1, 2)); // 0.95 agrees, 0.85 disagrees
        assert_eq!(r.conf90, (1, 1));
        assert_eq!(r.median_ms, Some(700));
        assert!((r.cost - 0.00003).abs() < 1e-12);

        let only = summarize(&rows, Some("sdd_judge"));
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].site, "sdd_judge");
    }

    #[test]
    fn render_handles_empty_and_zero_denominators() {
        assert_eq!(render(&[]), "no shadow data yet\n");
        let rows = vec![Row::failed("skill_rerank", "x", "a", "no key")];
        let text = render(&summarize(&rows, None));
        assert!(text.contains("skill_rerank") && text.contains("agreement      n/a"));
    }
}
