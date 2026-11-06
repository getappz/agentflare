//! Shared evidence and coverage envelope for insights reports.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::capability::AdapterCapabilities;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricBasis {
    Measured,
    Estimated { assumption: String },
    Inferred { rule: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub source: String,
    pub session_id: String,
    pub turn_seq: Option<u32>,
    pub tool_call_id: Option<String>,
    pub at: Option<DateTime<Utc>>,
    pub excerpt: Option<String>,
}

impl EvidenceRef {
    /// Keep at most 240 Unicode scalar values, including the truncation marker.
    pub fn with_excerpt(mut self, text: &str) -> Self {
        let mut chars = text.chars();
        let mut excerpt: String = chars.by_ref().take(240).collect();
        if chars.next().is_some() {
            excerpt.pop();
            excerpt.push('…');
        }
        self.excerpt = Some(excerpt);
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoverageMeta {
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub sessions_in_window: usize,
    pub eligible_sessions: usize,
    pub eligible_turns: usize,
    pub unpriced_sessions: usize,
    /// Count of priced sessions per `cost_basis` (`provider`/`estimated`/`unknown`).
    #[serde(default)]
    pub cost_basis_mix: std::collections::BTreeMap<String, usize>,
    pub sessions_missing_turn_timestamps: usize,
    pub failed_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub detector: String,
    pub value: Option<serde_json::Value>,
    pub unit: String,
    pub basis: MetricBasis,
    pub unavailable_reason: Option<String>,
    pub evidence: Vec<EvidenceRef>,
    pub note: String,
}

fn schema_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report<T> {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    pub detector_version: u32,
    pub generated_at: DateTime<Utc>,
    pub filters: serde_json::Value,
    pub coverage: CoverageMeta,
    pub capabilities: Vec<AdapterCapabilities>,
    pub body: T,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_round_trips_with_capabilities_and_defaults_schema_version() {
        let mut value = serde_json::json!({
            "detector_version": 1,
            "generated_at": "2026-01-02T00:00:00Z",
            "filters": {},
            "coverage": {
                "window_start": "2026-01-01T00:00:00Z",
                "window_end": "2026-01-02T00:00:00Z",
                "sessions_in_window": 0, "eligible_sessions": 0,
                "eligible_turns": 0, "unpriced_sessions": 0, "cost_basis_mix": {},
                "sessions_missing_turn_timestamps": 0, "failed_sources": []
            },
            "capabilities": crate::capability::capabilities(),
            "body": [{
                "id": "cache", "detector": "cache_reuse", "value": null,
                "unit": "share", "basis": "measured",
                "unavailable_reason": "no_eligible_sessions", "evidence": [], "note": ""
            }]
        });
        let report: Report<Vec<Finding>> = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(report.schema_version, 1);
        value["schema_version"] = 1.into();
        assert_eq!(serde_json::to_value(report).unwrap(), value);
        for basis in [
            MetricBasis::Estimated {
                assumption: "4 bytes per token".into(),
            },
            MetricBasis::Inferred {
                rule: "exit status".into(),
            },
        ] {
            let value = serde_json::to_value(&basis).unwrap();
            let decoded: MetricBasis = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), value);
        }
    }

    #[test]
    fn excerpt_truncates_on_char_boundary_to_240() {
        let reference = || EvidenceRef {
            source: "codex".into(),
            session_id: "session".into(),
            turn_seq: None,
            tool_call_id: None,
            at: None,
            excerpt: None,
        };
        for size in [0, 239, 240, 241, 500] {
            let text = "界".repeat(size);
            let excerpt = reference().with_excerpt(&text).excerpt.unwrap();
            assert_eq!(excerpt.chars().count(), size.min(240));
            if size > 240 {
                assert_eq!(excerpt, format!("{}…", "界".repeat(239)));
            } else {
                assert_eq!(excerpt, text);
            }
        }
    }
}
