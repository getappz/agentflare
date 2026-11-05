//! Go/no-go thresholds, set *before* measuring. TOML; every key optional.
//! See `docs/decide-bench-thresholds.toml` for the documented example.
use super::SiteReport;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteThresholds {
    pub min_agreement: Option<f64>,
    pub min_precision: Option<f64>,
    pub min_recall: Option<f64>,
    pub max_cold_p95_ms: Option<f64>,
    pub max_warm_p95_ms: Option<f64>,
    pub max_usd_per_1k: Option<f64>,
    pub max_overhead_pct: Option<f64>,
    /// Require every failure mode to fall back to the baseline (default true).
    pub require_fail_open: Option<bool>,
    /// Below this confidence Jev's answer is ignored (default 0.7).
    pub min_confidence: Option<f64>,
    /// LLM tokens one applied Jev decision saves (default 0).
    pub llm_tokens_avoided: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
    #[serde(default)]
    pub default: SiteThresholds,
    #[serde(default)]
    pub sites: BTreeMap<String, SiteThresholds>,
}

#[derive(Debug, Default, Serialize)]
pub struct Verdict {
    pub go: bool,
    /// Violated checks; empty when `go`.
    pub violations: Vec<String>,
}

impl Thresholds {
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// Site values override `[default]` key by key.
    pub fn for_site(&self, site: &str) -> SiteThresholds {
        let d = &self.default;
        let s = self.sites.get(site).cloned().unwrap_or_default();
        SiteThresholds {
            min_agreement: s.min_agreement.or(d.min_agreement),
            min_precision: s.min_precision.or(d.min_precision),
            min_recall: s.min_recall.or(d.min_recall),
            max_cold_p95_ms: s.max_cold_p95_ms.or(d.max_cold_p95_ms),
            max_warm_p95_ms: s.max_warm_p95_ms.or(d.max_warm_p95_ms),
            max_usd_per_1k: s.max_usd_per_1k.or(d.max_usd_per_1k),
            max_overhead_pct: s.max_overhead_pct.or(d.max_overhead_pct),
            require_fail_open: s.require_fail_open.or(d.require_fail_open),
            min_confidence: s.min_confidence.or(d.min_confidence),
            llm_tokens_avoided: s.llm_tokens_avoided.or(d.llm_tokens_avoided),
        }
    }
}

impl SiteThresholds {
    pub fn min_confidence(&self) -> f64 {
        self.min_confidence.unwrap_or(0.7)
    }

    pub fn llm_tokens_avoided(&self) -> u64 {
        self.llm_tokens_avoided.unwrap_or(0)
    }

    /// A set threshold with no measurement behind it is a violation.
    pub fn judge(&self, r: &SiteReport) -> Verdict {
        let mut v = vec![];
        let mut check = |name: &str, want: Option<f64>, got: Option<f64>, at_least: bool| {
            let Some(w) = want else { return };
            match got {
                Some(g) if (g >= w) == at_least => {}
                Some(g) => v.push(format!(
                    "{name} {g:.3} {} {w:.3}",
                    if at_least { "<" } else { ">" }
                )),
                None => v.push(format!("{name} unmeasured")),
            }
        };
        check("agreement", self.min_agreement, r.agreement, true);
        check("precision", self.min_precision, r.jev.precision, true);
        check("recall", self.min_recall, r.jev.recall, true);
        check(
            "cold p95 ms",
            self.max_cold_p95_ms,
            r.latency_cold.p95_ms,
            false,
        );
        check(
            "warm p95 ms",
            self.max_warm_p95_ms,
            r.latency_warm.p95_ms,
            false,
        );
        check("usd per 1k", self.max_usd_per_1k, r.usd_per_1k, false);
        check("overhead %", self.max_overhead_pct, r.overhead_pct, false);
        if self.require_fail_open.unwrap_or(true) {
            for f in &r.failure_modes {
                if f.fail_open_rate.is_some_and(|x| x < 1.0) {
                    v.push(format!("failure mode {} did not fail open", f.mode));
                }
            }
        }
        Verdict {
            go: v.is_empty(),
            violations: v,
        }
    }
}
