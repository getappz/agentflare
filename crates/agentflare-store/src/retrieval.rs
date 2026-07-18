//! Pure retrieval math shared by memory/search consumers. Ported from
//! openpawz's memory engine (merge/decay/MMR semantics). No I/O, no deps.

use std::collections::HashMap;
use std::hash::Hash;

/// Weighted merge of BM25 and vector hits. BM25 scores are min-max normalized
/// to [0,1]; a single result (or all-equal scores) normalizes to 1.0 — it IS
/// the best match, not the worst. Vector scores are assumed to already be
/// cosine similarities in [0,1]. Callers must pass HIGHER-IS-BETTER bm25
/// scores (negate SQLite's bm25() rank before calling).
pub fn merge_ranked<I: Eq + Hash + Clone>(
    bm25: &[(I, f64)],
    vec: &[(I, f64)],
    bm25_weight: f64,
    vec_weight: f64,
) -> Vec<(I, f64)> {
    let mut map: HashMap<I, (Option<f64>, Option<f64>)> = HashMap::new();
    let max = bm25.iter().map(|(_, s)| *s).fold(f64::MIN, f64::max);
    let min = bm25.iter().map(|(_, s)| *s).fold(f64::MAX, f64::min);
    let range = max - min;
    for (id, s) in bm25 {
        let n = if range.abs() < 1e-12 {
            1.0
        } else {
            (s - min) / range
        };
        map.entry(id.clone()).or_insert((None, None)).0 = Some(n);
    }
    for (id, s) in vec {
        map.entry(id.clone()).or_insert((None, None)).1 = Some(*s);
    }
    map.into_iter()
        .map(|(id, (b, v))| {
            (
                id,
                b.unwrap_or(0.0) * bm25_weight + v.unwrap_or(0.0) * vec_weight,
            )
        })
        .collect()
}

/// Exponential decay: 1.0 at age 0, 0.5 at one half-life.
pub fn decay_factor(age_days: f64, half_life_days: f64) -> f64 {
    (-(2.0f64.ln() / half_life_days) * age_days).exp()
}

/// Maximal Marginal Relevance selection over candidate indices.
/// `lambda`: 1.0 = pure relevance, 0.0 = pure diversity.
/// `sim(i, j)` returns similarity between candidates i and j in [0,1].
/// Returns selected indices in pick order (best first).
pub fn mmr_select(
    scores: &[f64],
    k: usize,
    lambda: f64,
    sim: impl Fn(usize, usize) -> f64,
) -> Vec<usize> {
    if scores.is_empty() || k == 0 {
        return Vec::new();
    }
    let mut remaining: Vec<usize> = (0..scores.len()).collect();
    remaining.sort_by(|a, b| {
        scores[*b]
            .partial_cmp(&scores[*a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut selected = vec![remaining.remove(0)];
    while selected.len() < k && !remaining.is_empty() {
        let (best_pos, _) = remaining
            .iter()
            .enumerate()
            .map(|(pos, &cand)| {
                let max_sim = selected
                    .iter()
                    .map(|&s| sim(cand, s))
                    .fold(0.0f64, f64::max);
                (pos, lambda * scores[cand] - (1.0 - lambda) * max_sim)
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .expect("remaining is non-empty");
        selected.push(remaining.remove(best_pos));
    }
    selected
}

/// Word-level Jaccard overlap; words shorter than 3 chars are ignored.
/// Returns 1.0 when both texts have no qualifying words (identical-as-empty).
pub fn jaccard_overlap(a: &str, b: &str) -> f64 {
    fn words(s: &str) -> std::collections::HashSet<&str> {
        s.split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
            .filter(|w| w.len() > 2)
            .collect()
    }
    let (wa, wb) = (words(a), words(b));
    if wa.is_empty() && wb.is_empty() {
        return 1.0;
    }
    let union = wa.union(&wb).count() as f64;
    if union < 1.0 {
        return 0.0;
    }
    wa.intersection(&wb).count() as f64 / union
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_bm25_result_normalizes_to_one() {
        // openpawz §6.3: one result (range 0) IS the best match → 1.0, not 0.0
        let merged = merge_ranked(&[("a", -1.2f64)], &[], 0.4, 0.6);
        assert_eq!(merged.len(), 1);
        assert!((merged[0].1 - 0.4).abs() < 1e-9); // 1.0 * bm25_weight
    }

    #[test]
    fn overlapping_ids_combine_both_scores() {
        let bm25 = vec![("a", 2.0f64), ("b", 1.0)];
        let vec_hits = vec![("a", 0.9f64), ("c", 0.5)];
        let merged = merge_ranked(&bm25, &vec_hits, 0.4, 0.6);
        let get = |id: &str| merged.iter().find(|(i, _)| *i == id).unwrap().1;
        // "a": bm25 max → norm 1.0 → 0.4, plus vector 0.9*0.6 = 0.54 → 0.94
        assert!((get("a") - 0.94).abs() < 1e-9);
        // "b": bm25 min → norm 0.0, no vector → 0.0
        assert!(get("b").abs() < 1e-9);
        // "c": vector only → 0.5*0.6 = 0.30
        assert!((get("c") - 0.30).abs() < 1e-9);
    }

    #[test]
    fn decay_halves_at_half_life() {
        assert!((decay_factor(30.0, 30.0) - 0.5).abs() < 1e-9);
        assert!((decay_factor(0.0, 30.0) - 1.0).abs() < 1e-9);
        assert!(decay_factor(300.0, 30.0) < 0.001);
    }

    #[test]
    fn mmr_lambda_one_is_pure_relevance_order() {
        let scores = [0.1, 0.9, 0.5];
        let picked = mmr_select(&scores, 3, 1.0, |_, _| 0.0);
        assert_eq!(picked, vec![1, 2, 0]);
    }

    #[test]
    fn mmr_penalizes_redundant_items() {
        // idx 0 best; idx 1 nearly identical to 0; idx 2 lower-scored but diverse.
        let scores = [1.0, 0.95, 0.6];
        let sim = |i: usize, j: usize| {
            let pair = (i.min(j), i.max(j));
            if pair == (0, 1) { 1.0 } else { 0.0 }
        };
        let picked = mmr_select(&scores, 2, 0.7, sim);
        assert_eq!(picked, vec![0, 2]); // diverse idx 2 beats redundant idx 1
    }

    #[test]
    fn mmr_k_zero_and_empty_are_empty() {
        assert!(mmr_select(&[], 5, 0.7, |_, _| 0.0).is_empty());
        assert!(mmr_select(&[1.0], 0, 0.7, |_, _| 0.0).is_empty());
    }

    #[test]
    fn jaccard_edges() {
        assert_eq!(jaccard_overlap("", ""), 1.0); // both empty = identical
        assert_eq!(jaccard_overlap("alpha beta", "gamma delta"), 0.0);
        assert_eq!(jaccard_overlap("alpha beta", "beta alpha"), 1.0);
        // words with len <= 2 are ignored
        assert_eq!(jaccard_overlap("of at alpha", "alpha in on"), 1.0);
    }
}
