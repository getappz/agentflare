use serde::Serialize;
use std::collections::BTreeSet;

/// The one token estimator used by every site: `ceil(chars / 4)`.
pub fn count_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

/// Nearest-rank percentile (`p` in 0..=100) of unsorted samples.
pub fn percentile(samples: &[u64], p: f64) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    let mut s = samples.to_vec();
    s.sort_unstable();
    let rank = ((p / 100.0) * s.len() as f64).ceil().max(1.0) as usize;
    Some(s[rank - 1])
}

#[derive(Debug, Default, Serialize)]
pub struct Pct {
    pub n: usize,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
}

impl Pct {
    pub fn of(samples_us: &[u64]) -> Self {
        let ms = |p| percentile(samples_us, p).map(|u| u as f64 / 1000.0);
        Self {
            n: samples_us.len(),
            p50_ms: ms(50.0),
            p95_ms: ms(95.0),
        }
    }
}

/// Accuracy plus macro precision/recall over the expected classes.
#[derive(Debug, Default, Serialize)]
pub struct Prf {
    pub n: usize,
    pub accuracy: Option<f64>,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
}

impl Prf {
    /// `pairs` are `(predicted, expected)`.
    pub fn of(pairs: &[(String, String)]) -> Self {
        if pairs.is_empty() {
            return Self::default();
        }
        let classes: BTreeSet<&str> = pairs.iter().map(|(_, e)| e.as_str()).collect();
        let (mut ps, mut rs) = (0.0, 0.0);
        for c in &classes {
            let tp = pairs.iter().filter(|(p, e)| p == c && e == c).count() as f64;
            let predicted = pairs.iter().filter(|(p, _)| p == c).count() as f64;
            let actual = pairs.iter().filter(|(_, e)| e == c).count() as f64;
            ps += if predicted > 0.0 { tp / predicted } else { 0.0 };
            rs += tp / actual;
        }
        let k = classes.len() as f64;
        let correct = pairs.iter().filter(|(p, e)| p == e).count();
        Self {
            n: pairs.len(),
            accuracy: Some(correct as f64 / pairs.len() as f64),
            precision: Some(ps / k),
            recall: Some(rs / k),
        }
    }
}
