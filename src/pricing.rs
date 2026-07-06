// Model pricing table, tiered-cost calculation, and alias/family-fallback
// model resolution — ported from claude-view (https://github.com/tombelieber/claude-view),
// MIT licensed. See /NOTICE for the full license text and attribution.
use std::collections::HashMap;
use std::sync::OnceLock;

pub(crate) const PRICING_JSON: &str = include_str!("../data/anthropic-pricing.json");
const PER_MTOK: f64 = 1_000_000.0;
const TIER_THRESHOLD: i64 = 200_000;

/// Per-model pricing in USD per token.
#[derive(Debug, Clone)]
pub struct ModelPricing {
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub cache_creation_cost_per_token: f64,
    pub cache_read_cost_per_token: f64,
    pub input_cost_per_token_above_200k: Option<f64>,
    pub output_cost_per_token_above_200k: Option<f64>,
    pub cache_creation_cost_per_token_above_200k: Option<f64>,
    pub cache_read_cost_per_token_above_200k: Option<f64>,
    pub cache_creation_cost_per_token_1hr: Option<f64>,
}

/// Token counts for one aggregation bucket (e.g. one model, one day).
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_creation_5m_tokens: u64,
    pub cache_creation_1hr_tokens: u64,
}

/// Cost in USD for one token snapshot.
#[derive(Debug, Clone, Default)]
pub struct CostBreakdown {
    pub total_usd: f64,
    pub has_unpriced_usage: bool,
}

#[derive(serde::Deserialize)]
struct PricingFile {
    models: HashMap<String, JsonModel>,
    #[serde(default)]
    aliases: HashMap<String, String>,
}

#[derive(serde::Deserialize)]
struct JsonModel {
    input: f64,
    output: f64,
    cache_write_5m: f64,
    cache_write_1hr: f64,
    cache_read: f64,
    long_context_pricing: Option<LongContextPricing>,
}

#[derive(serde::Deserialize)]
struct LongContextPricing {
    input: f64,
    output: f64,
    cache_write_5m: f64,
    cache_read: f64,
}

static PRICING_CACHE: OnceLock<HashMap<String, ModelPricing>> = OnceLock::new();

/// Load the embedded pricing table. Parsed once, then cloned from cache.
pub fn load_pricing() -> HashMap<String, ModelPricing> {
    PRICING_CACHE
        .get_or_init(|| {
            parse_pricing_file(PRICING_JSON)
                .expect("embedded anthropic-pricing.json is invalid")
        })
        .clone()
}

fn parse_pricing_file(json: &str) -> Result<HashMap<String, ModelPricing>, serde_json::Error> {
    let file: PricingFile = serde_json::from_str(json)?;
    let mut map = HashMap::with_capacity(file.models.len() + file.aliases.len());
    for (model_id, jm) in &file.models {
        map.insert(model_id.clone(), convert_model(jm));
    }
    for (alias, target) in &file.aliases {
        if let Some(mp) = map.get(target) {
            map.insert(alias.clone(), mp.clone());
        }
    }
    Ok(map)
}

fn convert_model(jm: &JsonModel) -> ModelPricing {
    let (above_input, above_output, above_cache_create, above_cache_read) =
        match &jm.long_context_pricing {
            Some(lcp) => (
                Some(lcp.input / PER_MTOK),
                Some(lcp.output / PER_MTOK),
                Some(lcp.cache_write_5m / PER_MTOK),
                Some(lcp.cache_read / PER_MTOK),
            ),
            None => (None, None, None, None),
        };

    ModelPricing {
        input_cost_per_token: jm.input / PER_MTOK,
        output_cost_per_token: jm.output / PER_MTOK,
        cache_creation_cost_per_token: jm.cache_write_5m / PER_MTOK,
        cache_read_cost_per_token: jm.cache_read / PER_MTOK,
        input_cost_per_token_above_200k: above_input,
        output_cost_per_token_above_200k: above_output,
        cache_creation_cost_per_token_above_200k: above_cache_create,
        cache_read_cost_per_token_above_200k: above_cache_read,
        cache_creation_cost_per_token_1hr: Some(jm.cache_write_1hr / PER_MTOK),
    }
}

/// Claude model family — the pricing-relevant grouping. Pricing has
/// historically been stable *within* a family across point releases, which is
/// what makes family-nearest fallback safe for a brand-new release whose exact
/// rate isn't in the table yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Fable,
    Opus,
    Sonnet,
    Haiku,
}

impl Family {
    fn from_token(token: &str) -> Option<Self> {
        match token {
            "fable" => Some(Family::Fable),
            "opus" => Some(Family::Opus),
            "sonnet" => Some(Family::Sonnet),
            "haiku" => Some(Family::Haiku),
            _ => None,
        }
    }
}

fn resolve_model_alias(alias: &str) -> Option<&'static str> {
    match alias {
        "haiku" => Some("claude-haiku-4-5-20251001"),
        "sonnet" => Some("claude-sonnet-4-6"),
        "opus" => Some("claude-opus-4-6"),
        _ => None,
    }
}

fn parse_claude_model(model_id: &str) -> Option<(Family, (u32, u32))> {
    let lower = model_id.to_ascii_lowercase();
    if !lower.starts_with("claude") {
        return None;
    }
    let mut family: Option<Family> = None;
    let mut versions: Vec<u32> = Vec::new();
    for token in lower.split('-') {
        if let Some(f) = Family::from_token(token) {
            family = Some(f);
        } else if let Ok(n) = token.parse::<u32>() {
            if n < 1000 {
                versions.push(n);
            }
        }
    }
    let family = family?;
    let major = versions.first().copied().unwrap_or(0);
    let minor = versions.get(1).copied().unwrap_or(0);
    Some((family, (major, minor)))
}

fn family_nearest_pricing<'a>(
    model_id: &str,
    pricing: &'a HashMap<String, ModelPricing>,
) -> Option<&'a ModelPricing> {
    let (want_family, want_version) = parse_claude_model(model_id)?;

    let mut candidates: Vec<((u32, u32), &ModelPricing)> = pricing
        .iter()
        .filter_map(|(key, p)| {
            let (family, version) = parse_claude_model(key)?;
            (family == want_family && version != (0, 0)).then_some((version, p))
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));

    candidates
        .iter()
        .rev()
        .find(|(version, _)| *version <= want_version)
        .or_else(|| candidates.first())
        .map(|(_, p)| *p)
}

/// Resolve pricing for a model id. Resolution order: exact -> alias -> prefix
/// (either direction) -> family-nearest-version fallback. Only genuinely
/// foreign ids (no Claude family) return `None` — never fabricates a rate for
/// a non-Claude model.
pub fn lookup_pricing<'a>(
    model_id: &str,
    pricing: &'a HashMap<String, ModelPricing>,
) -> Option<&'a ModelPricing> {
    if let Some(p) = pricing.get(model_id) {
        return Some(p);
    }
    if let Some(resolved) = resolve_model_alias(model_id) {
        if let Some(p) = pricing.get(resolved) {
            return Some(p);
        }
    }
    for (key, p) in pricing {
        if model_id.starts_with(key.as_str()) {
            return Some(p);
        }
    }
    {
        let matches: Vec<&ModelPricing> = pricing
            .iter()
            .filter(|(key, _)| key.starts_with(model_id))
            .map(|(_, p)| p)
            .collect();
        if let Some(first) = matches.first() {
            let consistent = matches.iter().all(|p| {
                p.input_cost_per_token == first.input_cost_per_token
                    && p.output_cost_per_token == first.output_cost_per_token
                    && p.cache_read_cost_per_token == first.cache_read_cost_per_token
                    && p.cache_creation_cost_per_token == first.cache_creation_cost_per_token
            });
            if consistent {
                return Some(first);
            }
        }
    }
    family_nearest_pricing(model_id, pricing)
}

pub fn tiered_cost(tokens: i64, base_rate: f64, above_200k_rate: Option<f64>) -> f64 {
    if tokens <= 0 {
        return 0.0;
    }
    match above_200k_rate {
        Some(high_rate) if tokens > TIER_THRESHOLD => {
            let below = TIER_THRESHOLD as f64 * base_rate;
            let above = (tokens - TIER_THRESHOLD) as f64 * high_rate;
            below + above
        }
        _ => tokens as f64 * base_rate,
    }
}

/// Calculate cost for a token snapshot using model-specific pricing. If
/// `model` is `None` or not found, USD is left at zero and `has_unpriced_usage`
/// is set (never converted using a fabricated fallback rate).
pub fn calculate_cost(
    tokens: &TokenUsage,
    model: Option<&str>,
    pricing: &HashMap<String, ModelPricing>,
) -> CostBreakdown {
    let model_pricing = model.and_then(|m| lookup_pricing(m, pricing));

    match model_pricing {
        Some(mp) => {
            let input_cost_usd = tiered_cost(
                tokens.input_tokens as i64,
                mp.input_cost_per_token,
                mp.input_cost_per_token_above_200k,
            );
            let output_cost_usd = tiered_cost(
                tokens.output_tokens as i64,
                mp.output_cost_per_token,
                mp.output_cost_per_token_above_200k,
            );
            let cache_read_cost_usd = tiered_cost(
                tokens.cache_read_tokens as i64,
                mp.cache_read_cost_per_token,
                mp.cache_read_cost_per_token_above_200k,
            );
            let cache_creation_cost_usd = {
                let has_split =
                    tokens.cache_creation_5m_tokens > 0 || tokens.cache_creation_1hr_tokens > 0;
                if has_split {
                    let cost_5m = tiered_cost(
                        tokens.cache_creation_5m_tokens as i64,
                        mp.cache_creation_cost_per_token,
                        mp.cache_creation_cost_per_token_above_200k,
                    );
                    let cost_1hr = match mp.cache_creation_cost_per_token_1hr {
                        Some(rate_1hr) => tokens.cache_creation_1hr_tokens as f64 * rate_1hr,
                        None => tiered_cost(
                            tokens.cache_creation_1hr_tokens as i64,
                            mp.cache_creation_cost_per_token,
                            mp.cache_creation_cost_per_token_above_200k,
                        ),
                    };
                    cost_5m + cost_1hr
                } else {
                    tiered_cost(
                        tokens.cache_creation_tokens as i64,
                        mp.cache_creation_cost_per_token,
                        mp.cache_creation_cost_per_token_above_200k,
                    )
                }
            };

            CostBreakdown {
                total_usd: input_cost_usd
                    + output_cost_usd
                    + cache_read_cost_usd
                    + cache_creation_cost_usd,
                has_unpriced_usage: false,
            }
        }
        None => {
            let unpriced_total = tokens.input_tokens
                + tokens.output_tokens
                + tokens.cache_read_tokens
                + tokens.cache_creation_tokens;
            CostBreakdown {
                has_unpriced_usage: unpriced_total > 0,
                ..Default::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pricing() -> HashMap<String, ModelPricing> {
        load_pricing()
    }

    #[test]
    fn test_load_pricing_parses_all_models() {
        let pricing = pricing();
        assert_eq!(pricing.len(), 22);
    }

    #[test]
    fn test_opus_46_flat_pricing() {
        let pricing = pricing();
        let tokens = TokenUsage { input_tokens: 500_000, ..Default::default() };
        let cost = calculate_cost(&tokens, Some("claude-opus-4-6"), &pricing);
        assert!((cost.total_usd - 2.50).abs() < 0.001);
        assert!(!cost.has_unpriced_usage);
    }

    #[test]
    fn test_sonnet_45_tiered_pricing() {
        let pricing = pricing();
        let tokens = TokenUsage { input_tokens: 500_000, ..Default::default() };
        let cost = calculate_cost(&tokens, Some("claude-sonnet-4-5-20250929"), &pricing);
        assert!((cost.total_usd - 2.40).abs() < 0.001);
    }

    #[test]
    fn test_unknown_model_has_no_fake_usd() {
        let pricing = pricing();
        let tokens = TokenUsage { input_tokens: 1_000_000, ..Default::default() };
        let cost = calculate_cost(&tokens, Some("gpt-4o"), &pricing);
        assert_eq!(cost.total_usd, 0.0);
        assert!(cost.has_unpriced_usage);
    }

    #[test]
    fn test_zero_tokens() {
        let pricing = pricing();
        let cost = calculate_cost(&TokenUsage::default(), Some("claude-opus-4-6"), &pricing);
        assert_eq!(cost.total_usd, 0.0);
        assert!(!cost.has_unpriced_usage);
    }

    #[test]
    fn test_1hr_cache_tokens_use_higher_rate() {
        let pricing = pricing();
        let tokens = TokenUsage {
            cache_creation_tokens: 100_000,
            cache_creation_1hr_tokens: 100_000,
            ..Default::default()
        };
        let cost = calculate_cost(&tokens, Some("claude-opus-4-6"), &pricing);
        // 100k tokens at the 1hr rate ($10/MTok) = $1.00
        assert!((cost.total_usd - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_resolve_model_alias_haiku_sonnet_opus() {
        let pricing = pricing();
        assert!(lookup_pricing("haiku", &pricing).is_some());
        assert!(lookup_pricing("sonnet", &pricing).is_some());
        assert!(lookup_pricing("opus", &pricing).is_some());
    }

    #[test]
    fn test_prefix_lookup_sonnet_46_dated() {
        let pricing = pricing();
        assert!(lookup_pricing("claude-sonnet-4-6-20260301", &pricing).is_some());
    }

    #[test]
    fn test_future_point_release_resolves_via_family_fallback() {
        let pricing = pricing();
        let opus_future = lookup_pricing("claude-opus-4-99", &pricing)
            .expect("future opus point release must resolve via family fallback");
        let opus_latest = pricing.get("claude-opus-4-7").unwrap();
        assert_eq!(
            opus_future.input_cost_per_token,
            opus_latest.input_cost_per_token
        );
    }

    #[test]
    fn test_foreign_model_never_fabricates_rate() {
        let pricing = pricing();
        assert!(lookup_pricing("gpt-4o", &pricing).is_none());
        assert!(lookup_pricing("unknown-model", &pricing).is_none());
    }
}
