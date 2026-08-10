//! Provider registry: embedded defaults + a locally-cached, on-demand-
//! refreshed remote copy. Adding a provider — another OpenAI-compatible
//! endpoint, another Cloudflare AI Gateway route — is a
//! `registry/providers.toml` edit upstream that ships to every existing
//! install on its next refresh, not a Rust change *or* a new release.
//!
//! Pattern ported from appz-cli's `registry/toolchains.toml` +
//! `crates/appz-core/src/{toolchain_registry,registry_cache}.rs`, scoped
//! down to what flare-proxy needs: no offline-mode CLI flag, no generic
//! `<T>` cache helper for other registries to share (single call site).
//!
//! `registry()` — used by `find`/`resolve`/`known_prefixes`, and by this
//! module's own tests — only ever reads the embedded TOML plus whatever is
//! already on disk in the local cache. It never makes a network call, so
//! it's fast and deterministic everywhere, including `cargo test`.
//! `ensure_fresh()` is the separate, explicit "download on demand" step:
//! call it once at proxy startup, before the first `registry()` access —
//! if the local cache is missing or older than `CACHE_TTL_SECONDS`, it
//! attempts one best-effort live fetch (short timeout, silent fallback to
//! embedded-only on any failure) and writes the result to disk for next
//! time.

use super::ProviderKind;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const REGISTRY_TOML: &str = include_str!("../../registry/providers.toml");
const REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/getappz/agentflare/main/crates/flare-proxy/registry/providers.toml";
const CACHE_TTL_SECONDS: u64 = 24 * 3600;
const FETCH_TIMEOUT_SECS: u64 = 3;

#[derive(Debug, Deserialize)]
struct RegistryFile {
    #[allow(dead_code)]
    version: u32,
    providers: Vec<RegistrySpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
struct CachedRegistry {
    fetched_at: u64,
    providers: Vec<RegistrySpec>,
}

fn registry() -> &'static [RegistrySpec] {
    static REGISTRY: OnceLock<Vec<RegistrySpec>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut specs = embedded_providers();
        if let Some(cached) = read_cache() {
            merge(&mut specs, cached.providers);
        }
        specs
    })
}

fn embedded_providers() -> Vec<RegistrySpec> {
    toml::from_str::<RegistryFile>(REGISTRY_TOML)
        .expect(
            "registry/providers.toml must parse — checked by provider_registry_toml_parses test",
        )
        .providers
}

/// Remote/cached entries override an embedded entry with the same
/// `prefix`, or are appended as a brand-new provider.
fn merge(embedded: &mut Vec<RegistrySpec>, overrides: Vec<RegistrySpec>) {
    for spec in overrides {
        if let Some(existing) = embedded.iter_mut().find(|s| s.prefix == spec.prefix) {
            *existing = spec;
        } else {
            embedded.push(spec);
        }
    }
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

/// "Download on demand": if the local cache is missing or stale, attempt
/// one live fetch and persist it — best-effort, short timeout, silently
/// falls back to embedded-only on any failure (offline, DNS, non-2xx,
/// malformed TOML). Call once at proxy startup, before the first
/// `registry()`/`find()`/`resolve()` use; `registry()` itself never
/// touches the network, so this is the only function in this module that
/// does.
pub fn ensure_fresh() {
    let now = now_secs();
    if !is_stale(now) {
        return;
    }
    let Ok(raw) = fetch_remote() else {
        return;
    };
    let Ok(providers) = parse_registry_toml(&raw) else {
        return;
    };
    write_cache(&CachedRegistry {
        fetched_at: now,
        providers,
    });
}

fn is_stale(now: u64) -> bool {
    read_cache()
        .and_then(|c| c.fetched_at.checked_add(CACHE_TTL_SECONDS))
        .map(|expires| now > expires)
        .unwrap_or(true)
}

fn fetch_remote() -> Result<String, String> {
    let resp = ureq::get(REGISTRY_URL)
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .call()
        .map_err(|e| e.to_string())?;
    resp.into_string().map_err(|e| e.to_string())
}

fn parse_registry_toml(raw: &str) -> Result<Vec<RegistrySpec>, toml::de::Error> {
    toml::from_str::<RegistryFile>(raw).map(|f| f.providers)
}

fn read_cache() -> Option<CachedRegistry> {
    let raw = std::fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_cache(payload: &CachedRegistry) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(payload) {
        let _ = std::fs::write(path, json);
    }
}

fn cache_path() -> PathBuf {
    agentflare_home().join("flare-proxy-providers-registry.json")
}

/// Mirrors `crate::paths::home()` in the main `agentflare` binary crate —
/// flare-proxy can't depend on that crate (agentflare depends on
/// flare-proxy, not the reverse), so this is a small local copy of the
/// same convention rather than a shared helper.
fn agentflare_home() -> PathBuf {
    let home = std::env::var("AGENTFLARE_HOME_OVERRIDE")
        .map(PathBuf::from)
        .or_else(|_| dirs::home_dir().ok_or(()))
        .unwrap_or_else(|_| PathBuf::from("."));
    home.join(".agentflare")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
    fn every_registry_entry_has_a_resolvable_static_or_gateway_url() {
        // Every entry without a base_url_template must have a plain
        // base_url -- catches a copy-paste mistake (e.g. a new provider
        // block missing both fields) at test time instead of at request
        // time via a confusing "no route" error.
        for spec in registry() {
            assert!(
                spec.base_url.is_some() || spec.base_url_template.is_some(),
                "provider '{}' has neither base_url nor base_url_template",
                spec.prefix
            );
        }
    }

    #[test]
    fn broad_openai_compatible_providers_are_present_and_resolve() {
        for prefix in [
            "openai",
            "deepseek",
            "groq",
            "mistral",
            "xai",
            "together",
            "perplexity",
            "fireworks",
            "deepinfra",
            "moonshot",
            "novita",
            "hyperbolic",
            "cerebras",
            "sambanova",
            "github_models",
            "cohere",
            "ollama",
            "siliconflow",
            "llm7",
            "requesty",
        ] {
            let spec =
                find(prefix).unwrap_or_else(|| panic!("registry must define provider '{prefix}'"));
            assert!(matches!(spec.kind, ProviderKind::OpenAiCompatible));
            let (base_url, _) =
                resolve(spec).unwrap_or_else(|| panic!("'{prefix}' must resolve to a base_url"));
            assert!(
                base_url.starts_with("http://") || base_url.starts_with("https://"),
                "'{prefix}' base_url '{base_url}' is not a valid URL prefix"
            );
        }
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
        assert_eq!(resolve_template("https://x/{A}/{B}/y"), None);
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

    #[test]
    fn merge_overrides_matching_prefix_and_appends_new_ones() {
        let mut embedded = vec![RegistrySpec {
            prefix: "nvidia_nim".into(),
            id: "nvidia-nim".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: Some("https://old.example".into()),
            base_url_template: None,
            api_key_env: None,
            extra_headers: vec![],
            gateway_auth_env: None,
            gateway_auth_header: None,
        }];
        let overrides = vec![
            RegistrySpec {
                prefix: "nvidia_nim".into(),
                id: "nvidia-nim".into(),
                kind: ProviderKind::OpenAiCompatible,
                base_url: Some("https://new.example".into()),
                base_url_template: None,
                api_key_env: None,
                extra_headers: vec![],
                gateway_auth_env: None,
                gateway_auth_header: None,
            },
            RegistrySpec {
                prefix: "brand_new".into(),
                id: "brand-new".into(),
                kind: ProviderKind::OpenAiCompatible,
                base_url: Some("https://brand-new.example".into()),
                base_url_template: None,
                api_key_env: None,
                extra_headers: vec![],
                gateway_auth_env: None,
                gateway_auth_header: None,
            },
        ];
        merge(&mut embedded, overrides);
        assert_eq!(embedded.len(), 2);
        assert_eq!(
            embedded
                .iter()
                .find(|s| s.prefix == "nvidia_nim")
                .unwrap()
                .base_url
                .as_deref(),
            Some("https://new.example")
        );
        assert!(embedded.iter().any(|s| s.prefix == "brand_new"));
    }

    #[test]
    fn is_stale_when_no_cache_file_present() {
        let _guard = crate::test_env_lock();
        let dir =
            std::env::temp_dir().join(format!("flare-proxy-registry-test-{}", std::process::id()));
        std::env::set_var("AGENTFLARE_HOME_OVERRIDE", &dir);
        assert!(is_stale(now_secs()));
        std::env::remove_var("AGENTFLARE_HOME_OVERRIDE");
    }

    #[test]
    fn cache_round_trip_is_not_stale_within_ttl() {
        let _guard = crate::test_env_lock();
        let dir = std::env::temp_dir().join(format!(
            "flare-proxy-registry-test-{}-fresh",
            std::process::id()
        ));
        std::env::set_var("AGENTFLARE_HOME_OVERRIDE", &dir);

        let now = now_secs();
        write_cache(&CachedRegistry {
            fetched_at: now,
            providers: vec![],
        });
        assert!(!is_stale(now));
        assert!(is_stale(now + CACHE_TTL_SECONDS + 1));

        let _ = std::fs::remove_dir_all(&dir);
        std::env::remove_var("AGENTFLARE_HOME_OVERRIDE");
    }
}
