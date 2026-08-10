//! Loads `registry/providers.toml` — the source of truth for which
//! providers flare-proxy knows about. Adding a provider (a new
//! OpenAI-compatible endpoint, a new Cloudflare AI Gateway route, ...) is a
//! TOML edit here, not a Rust change; `mod.rs` only turns a resolved spec
//! into a `ProviderEntry`.

use super::ProviderKind;
use serde::Deserialize;
use std::sync::OnceLock;

const REGISTRY_TOML: &str = include_str!("../../registry/providers.toml");

#[derive(Debug, Deserialize)]
struct RegistryFile {
    #[allow(dead_code)]
    version: u32,
    providers: Vec<RegistrySpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistrySpec {
    /// The prefix used in `MODEL`/`MODEL_OPUS`/... env vars, e.g.
    /// `nvidia_nim/meta/llama-...`.
    pub prefix: String,
    /// Internal provider id, used as `ModelRoute::provider_id` and by
    /// `ProviderConfig::provider()` lookups.
    pub id: String,
    pub kind: ProviderKind,
    #[serde(default)]
    pub base_url: Option<String>,
    /// A `base_url` with `{ENV_VAR_NAME}` placeholders, resolved from the
    /// process environment at lookup time — for providers whose URL is
    /// per-user (Cloudflare AI Gateway's account_id/gateway_id).
    #[serde(default)]
    pub base_url_template: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub extra_headers: Vec<(String, String)>,
    /// An extra header sourced from an env var, distinct from
    /// `api_key_env` — e.g. Cloudflare's `cf-aig-authorization`, which is
    /// gateway-level auth separate from the underlying provider's own key.
    #[serde(default)]
    pub gateway_auth_env: Option<String>,
    #[serde(default)]
    pub gateway_auth_header: Option<String>,
}

fn registry() -> &'static [RegistrySpec] {
    static REGISTRY: OnceLock<Vec<RegistrySpec>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        toml::from_str::<RegistryFile>(REGISTRY_TOML)
            .expect("registry/providers.toml must parse — checked by provider_registry_toml_parses test")
            .providers
    })
}

pub fn known_prefixes() -> Vec<&'static str> {
    registry().iter().map(|s| s.prefix.as_str()).collect()
}

pub fn find(prefix: &str) -> Option<&'static RegistrySpec> {
    registry().iter().find(|s| s.prefix == prefix)
}

/// Resolve a spec into a concrete `(base_url, extra_headers)` pair for the
/// current environment. Returns `None` when `base_url_template`
/// references an env var that isn't set — the provider is unavailable in
/// this environment, not misconfigured, so callers should treat that the
/// same as an unknown provider rather than panicking.
pub fn resolve(spec: &RegistrySpec) -> Option<(String, Vec<(String, String)>)> {
    let base_url = match (&spec.base_url, &spec.base_url_template) {
        (Some(url), _) => url.clone(),
        (None, Some(template)) => resolve_template(template)?,
        (None, None) => return None,
    };

    let mut extra_headers = spec.extra_headers.clone();
    if let (Some(env_var), Some(header)) = (&spec.gateway_auth_env, &spec.gateway_auth_header) {
        if let Ok(token) = std::env::var(env_var) {
            if !token.is_empty() {
                extra_headers.push((header.clone(), format!("Bearer {token}")));
            }
        }
    }

    Some((base_url, extra_headers))
}

fn resolve_template(template: &str) -> Option<String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        let end = rest.find('}')?;
        let var_name = &rest[..end];
        let value = std::env::var(var_name).ok().filter(|v| !v.is_empty())?;
        out.push_str(&value);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_registry_toml_parses() {
        // Forces the lazy parse now, at test time, instead of only ever
        // discovering a malformed registry/providers.toml when some other
        // test happens to call registry()/find()/known_prefixes() first.
        assert!(!registry().is_empty());
    }

    #[test]
    fn find_returns_known_static_provider() {
        let spec = find("nvidia_nim").expect("nvidia_nim must be in the registry");
        assert_eq!(spec.id, "nvidia-nim");
        assert!(matches!(spec.kind, ProviderKind::OpenAiCompatible));
    }

    #[test]
    fn find_returns_none_for_unknown_prefix() {
        assert!(find("made_up_provider").is_none());
    }

    #[test]
    fn resolve_static_base_url_ignores_env() {
        let spec = find("anthropic").unwrap();
        let (base_url, headers) = resolve(spec).unwrap();
        assert_eq!(base_url, "https://api.anthropic.com");
        assert!(headers.is_empty());
    }

    #[test]
    fn resolve_template_substitutes_multiple_placeholders() {
        assert_eq!(
            resolve_template("https://x/{A}/{B}/y"),
            None // env vars unset in this process by default
        );
        std::env::set_var("__FLARE_PROXY_TEST_A", "acct");
        std::env::set_var("__FLARE_PROXY_TEST_B", "gw");
        assert_eq!(
            resolve_template("https://x/{__FLARE_PROXY_TEST_A}/{__FLARE_PROXY_TEST_B}/y"),
            Some("https://x/acct/gw/y".to_string())
        );
        std::env::remove_var("__FLARE_PROXY_TEST_A");
        std::env::remove_var("__FLARE_PROXY_TEST_B");
    }

    #[test]
    fn resolve_returns_none_when_template_var_missing() {
        let _guard = crate::test_env_lock();
        let spec = find("cf_gateway_anthropic").unwrap();
        std::env::remove_var("CF_AI_GATEWAY_ACCOUNT_ID");
        std::env::remove_var("CF_AI_GATEWAY_ID");
        assert!(resolve(spec).is_none());
    }
}
