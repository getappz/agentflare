use serde::{Deserialize, Serialize};

pub mod anthropic;
pub mod gemini;
pub mod openai_compat;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub providers: Vec<ProviderEntry>,
    pub routing: Vec<ModelRoute>,
    pub model: Option<String>,
    pub model_opus: Option<String>,
    pub model_sonnet: Option<String>,
    pub model_haiku: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub id: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub api_key_env: Option<String>,
    pub default_model: Option<String>,
    pub models: Vec<ModelDef>,
    /// Extra static headers this provider always sends (e.g. OpenRouter's
    /// HTTP-Referer/X-Title). Distinct from per-request auth, which is
    /// derived from `api_key_env` by each provider module.
    #[serde(default)]
    pub extra_headers: Vec<(String, String)>,
}

/// Which upstream wire protocol a provider speaks. `OpenAiCompatible` covers
/// any endpoint implementing OpenAI's `/chat/completions` shape (NVIDIA NIM,
/// OpenRouter, LM Studio, and any future OpenAI-compatible endpoint —
/// including one pointed at a Cloudflare AI Gateway OpenAI-compat route —
/// need only a config entry here, not new code).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProviderKind {
    OpenAiCompatible,
    Anthropic,
    Gemini,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDef {
    pub id: String,
    pub upstream_model: String,
    pub max_input_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRoute {
    pub anthropic_model: String,
    pub provider_id: String,
    pub upstream_model: String,
    pub requires_heuristic_tools: bool,
    pub requires_think_parsing: bool,
}

fn parse_model_string(s: &str) -> (String, String) {
    match s.split_once('/') {
        Some((provider, model)) => (provider.to_string(), model.to_string()),
        None => ("nvidia_nim".into(), s.into()),
    }
}

fn unsupported_provider_type(provider_types: &[String]) -> Option<&str> {
    provider_types.iter().map(String::as_str).find(|pt| {
        !matches!(
            *pt,
            "nvidia_nim"
                | "open_router"
                | "lmstudio"
                | "anthropic"
                | "gemini"
                | "cf_gateway_anthropic"
                | "cf_gateway_openai"
        )
    })
}

/// Cloudflare AI Gateway is a URL prefix in front of a provider's own wire
/// protocol, not a distinct one — `.../anthropic` still speaks Anthropic's
/// real Messages API shape, `.../compat` still speaks OpenAI's
/// `/chat/completions` shape. So this needs no new `ProviderKind`, just a
/// base_url built from account_id/gateway_id and an optional gateway-auth
/// header, layered onto the existing Anthropic/OpenAiCompatible paths.
fn cloudflare_gateway_base_url(provider_segment: &str) -> Option<String> {
    let account_id = std::env::var("CF_AI_GATEWAY_ACCOUNT_ID").ok()?;
    let gateway_id = std::env::var("CF_AI_GATEWAY_ID").ok()?;
    if account_id.is_empty() || gateway_id.is_empty() {
        return None;
    }
    Some(format!(
        "https://gateway.ai.cloudflare.com/v1/{account_id}/{gateway_id}/{provider_segment}"
    ))
}

/// `cf-aig-authorization` is Cloudflare's own gateway-level auth, separate
/// from the underlying provider's API key (which still goes through the
/// normal `Authorization`/`x-api-key` header each provider module already
/// sends). Optional — a gateway with no auth configured just omits it.
fn cloudflare_gateway_headers() -> Vec<(String, String)> {
    match std::env::var("CF_AI_GATEWAY_TOKEN") {
        Ok(token) if !token.is_empty() => {
            vec![("cf-aig-authorization".into(), format!("Bearer {token}"))]
        }
        _ => vec![],
    }
}

impl ProviderConfig {
    pub fn from_env() -> Self {
        let default_model = std::env::var("MODEL").ok().filter(|s| !s.is_empty());

        let default_model = match default_model {
            Some(m) => m,
            None => return Self::default_free(),
        };

        let model_opus = std::env::var("MODEL_OPUS").ok().filter(|s| !s.is_empty());
        let model_sonnet = std::env::var("MODEL_SONNET").ok().filter(|s| !s.is_empty());
        let model_haiku = std::env::var("MODEL_HAIKU").ok().filter(|s| !s.is_empty());

        let all_model_strs: Vec<String> = std::iter::once(default_model.clone())
            .chain(model_opus.clone())
            .chain(model_sonnet.clone())
            .chain(model_haiku.clone())
            .collect();

        let mut provider_types: Vec<String> = Vec::new();
        for ms in &all_model_strs {
            let pt = ms.split('/').next().unwrap_or("").to_string();
            if !pt.is_empty() && !provider_types.contains(&pt) {
                provider_types.push(pt);
            }
        }

        // Fail fast on a typo'd provider prefix: without this, an unsupported
        // prefix silently drops out of `providers` (see the `_ => continue`
        // below) while the route referencing it survives, so misconfiguration
        // looks like a successful startup and only errors later, at request
        // time, with an opaque "unknown provider".
        if let Some(bad) = unsupported_provider_type(&provider_types) {
            eprintln!(
                "agentflare: unsupported provider prefix '{bad}' in MODEL/MODEL_OPUS/MODEL_SONNET/MODEL_HAIKU -- falling back to default_free() config. Supported prefixes: nvidia_nim, open_router, lmstudio, anthropic, gemini, cf_gateway_anthropic, cf_gateway_openai."
            );
            return Self::default_free();
        }

        let mut providers = Vec::new();
        for pt in &provider_types {
            let (base_url, api_key_env, kind, extra_headers): (String, Option<&str>, _, _) =
                match pt.as_str() {
                    "nvidia_nim" => (
                        "https://integrate.api.nvidia.com/v1".into(),
                        Some("NVIDIA_NIM_API_KEY"),
                        ProviderKind::OpenAiCompatible,
                        vec![],
                    ),
                    "open_router" => (
                        "https://openrouter.ai/api/v1".into(),
                        Some("OPENROUTER_API_KEY"),
                        ProviderKind::OpenAiCompatible,
                        openrouter_headers(),
                    ),
                    "lmstudio" => (
                        "http://localhost:1234/v1".into(),
                        None,
                        ProviderKind::OpenAiCompatible,
                        vec![],
                    ),
                    "anthropic" => (
                        "https://api.anthropic.com".into(),
                        Some("ANTHROPIC_API_KEY"),
                        ProviderKind::Anthropic,
                        vec![],
                    ),
                    "gemini" => (
                        "https://generativelanguage.googleapis.com/v1beta".into(),
                        Some("GEMINI_API_KEY"),
                        ProviderKind::Gemini,
                        vec![],
                    ),
                    "cf_gateway_anthropic" => {
                        let Some(base) = cloudflare_gateway_base_url("anthropic") else {
                            eprintln!(
                                "agentflare: cf_gateway_anthropic requires CF_AI_GATEWAY_ACCOUNT_ID and CF_AI_GATEWAY_ID -- falling back to default_free() config."
                            );
                            return Self::default_free();
                        };
                        (
                            base,
                            Some("ANTHROPIC_API_KEY"),
                            ProviderKind::Anthropic,
                            cloudflare_gateway_headers(),
                        )
                    }
                    "cf_gateway_openai" => {
                        let Some(base) = cloudflare_gateway_base_url("compat") else {
                            eprintln!(
                                "agentflare: cf_gateway_openai requires CF_AI_GATEWAY_ACCOUNT_ID and CF_AI_GATEWAY_ID -- falling back to default_free() config."
                            );
                            return Self::default_free();
                        };
                        (
                            base,
                            Some("OPENAI_API_KEY"),
                            ProviderKind::OpenAiCompatible,
                            cloudflare_gateway_headers(),
                        )
                    }
                    _ => continue,
                };

            providers.push(ProviderEntry {
                id: pt.clone(),
                kind,
                base_url,
                api_key_env: api_key_env.map(String::from),
                default_model: None,
                models: vec![],
                extra_headers,
            });
        }

        let mut routing = Vec::new();
        let (def_provider, def_model) = parse_model_string(&default_model);
        routing.push(ModelRoute {
            anthropic_model: String::new(),
            provider_id: def_provider,
            upstream_model: def_model,
            requires_heuristic_tools: true,
            requires_think_parsing: false,
        });

        for (keyword, opt) in [
            ("opus", &model_opus),
            ("sonnet", &model_sonnet),
            ("haiku", &model_haiku),
        ] {
            if let Some(m) = opt {
                let (pid, um) = parse_model_string(m);
                routing.push(ModelRoute {
                    anthropic_model: keyword.into(),
                    provider_id: pid,
                    upstream_model: um,
                    requires_heuristic_tools: true,
                    requires_think_parsing: false,
                });
            }
        }

        Self {
            providers,
            routing,
            model: Some(default_model),
            model_opus,
            model_sonnet,
            model_haiku,
        }
    }

    pub fn resolve_model(&self, anthropic_model: &str) -> Option<&ModelRoute> {
        let name_lower = anthropic_model.to_lowercase();
        for keyword in ["opus", "haiku", "sonnet"] {
            if name_lower.contains(keyword) {
                if let Some(route) = self.routing.iter().find(|r| r.anthropic_model == keyword) {
                    return Some(route);
                }
                break;
            }
        }
        self.routing
            .iter()
            .find(|r| r.anthropic_model == anthropic_model || r.anthropic_model.is_empty())
    }

    pub fn default_free() -> Self {
        Self {
            model: None,
            model_opus: None,
            model_sonnet: None,
            model_haiku: None,
            providers: vec![
                ProviderEntry {
                    id: "nvidia-nim".into(),
                    kind: ProviderKind::OpenAiCompatible,
                    base_url: "https://integrate.api.nvidia.com/v1".into(),
                    api_key_env: Some("NVIDIA_NIM_API_KEY".into()),
                    default_model: Some("meta/llama-3.1-405b-instruct".into()),
                    models: vec![
                        ModelDef {
                            id: "meta/llama-3.1-405b-instruct".into(),
                            upstream_model: "meta/llama-3.1-405b-instruct".into(),
                            max_input_tokens: Some(128_000),
                        },
                        ModelDef {
                            id: "meta/llama-3.3-70b-instruct".into(),
                            upstream_model: "meta/llama-3.3-70b-instruct".into(),
                            max_input_tokens: Some(128_000),
                        },
                    ],
                    extra_headers: vec![],
                },
                ProviderEntry {
                    id: "openrouter".into(),
                    kind: ProviderKind::OpenAiCompatible,
                    base_url: "https://openrouter.ai/api/v1".into(),
                    api_key_env: Some("OPENROUTER_API_KEY".into()),
                    default_model: None,
                    models: vec![ModelDef {
                        id: "openrouter/auto".into(),
                        upstream_model: "openrouter/auto".into(),
                        max_input_tokens: None,
                    }],
                    extra_headers: openrouter_headers(),
                },
                ProviderEntry {
                    id: "lm-studio".into(),
                    kind: ProviderKind::OpenAiCompatible,
                    base_url: "http://localhost:1234/v1".into(),
                    api_key_env: None,
                    default_model: Some("local-model".into()),
                    models: vec![ModelDef {
                        id: "local-model".into(),
                        upstream_model: "local-model".into(),
                        max_input_tokens: Some(32_000),
                    }],
                    extra_headers: vec![],
                },
            ],
            routing: vec![
                ModelRoute {
                    anthropic_model: "claude-sonnet-4-20250514".into(),
                    provider_id: "nvidia-nim".into(),
                    upstream_model: "meta/llama-3.1-405b-instruct".into(),
                    requires_heuristic_tools: true,
                    requires_think_parsing: false,
                },
                ModelRoute {
                    anthropic_model: "claude-sonnet-4-5-20250601".into(),
                    provider_id: "openrouter".into(),
                    upstream_model: "openrouter/auto".into(),
                    requires_heuristic_tools: false,
                    requires_think_parsing: false,
                },
                ModelRoute {
                    anthropic_model: "claude-haiku-3-5-20241022".into(),
                    provider_id: "lm-studio".into(),
                    upstream_model: "local-model".into(),
                    requires_heuristic_tools: true,
                    requires_think_parsing: true,
                },
            ],
        }
    }

    pub fn provider(&self, id: &str) -> Option<&ProviderEntry> {
        self.providers.iter().find(|p| p.id == id)
    }
}

fn openrouter_headers() -> Vec<(String, String)> {
    vec![
        ("HTTP-Referer".into(), "https://agentflare.dev".into()),
        ("X-Title".into(), "agentflare".into()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_string_splits_on_first_slash() {
        assert_eq!(
            parse_model_string("nvidia_nim/meta/llama-3.1-405b-instruct"),
            (
                "nvidia_nim".to_string(),
                "meta/llama-3.1-405b-instruct".to_string()
            )
        );
        assert_eq!(
            parse_model_string("bare-model"),
            ("nvidia_nim".to_string(), "bare-model".to_string())
        );
    }

    #[test]
    fn resolve_model_falls_back_to_default_free() {
        let config = ProviderConfig::default_free();
        let route = config.resolve_model("claude-sonnet-4-20250514").unwrap();
        assert_eq!(route.provider_id, "nvidia-nim");
    }

    #[test]
    fn resolve_model_prefers_keyword_route_when_present() {
        let mut config = ProviderConfig::default_free();
        config.routing.push(ModelRoute {
            anthropic_model: "opus".into(),
            provider_id: "openrouter".into(),
            upstream_model: "openrouter/opus-override".into(),
            requires_heuristic_tools: false,
            requires_think_parsing: false,
        });

        let route = config.resolve_model("claude-opus-4-20250514").unwrap();
        assert_eq!(route.upstream_model, "openrouter/opus-override");
    }

    #[test]
    fn resolve_model_falls_back_to_empty_default_when_keyword_route_missing() {
        let config = ProviderConfig {
            providers: vec![],
            routing: vec![ModelRoute {
                anthropic_model: String::new(),
                provider_id: "nvidia_nim".into(),
                upstream_model: "meta/llama-3.1-405b-instruct".into(),
                requires_heuristic_tools: true,
                requires_think_parsing: false,
            }],
            model: Some("nvidia_nim/meta/llama-3.1-405b-instruct".into()),
            model_opus: None,
            model_sonnet: None,
            model_haiku: None,
        };

        // No MODEL_OPUS route configured — substring match on "opus" must not
        // return None, it should fall through to the empty-keyed default route.
        let route = config.resolve_model("claude-opus-4-20250514").unwrap();
        assert_eq!(route.upstream_model, "meta/llama-3.1-405b-instruct");
    }

    #[test]
    fn unsupported_provider_type_detects_bad_prefix() {
        let types = vec!["nvidia_nim".to_string(), "made_up_provider".to_string()];
        assert_eq!(unsupported_provider_type(&types), Some("made_up_provider"));
    }

    #[test]
    fn unsupported_provider_type_accepts_all_known_prefixes() {
        let types = vec![
            "nvidia_nim".to_string(),
            "open_router".to_string(),
            "lmstudio".to_string(),
            "anthropic".to_string(),
            "gemini".to_string(),
        ];
        assert_eq!(unsupported_provider_type(&types), None);
    }

    #[test]
    fn from_env_anthropic_prefix_resolves_to_native_provider() {
        let _guard = crate::test_env_lock();
        std::env::set_var("MODEL", "anthropic/claude-sonnet-4-5");
        std::env::remove_var("MODEL_OPUS");
        std::env::remove_var("MODEL_SONNET");
        std::env::remove_var("MODEL_HAIKU");

        let config = ProviderConfig::from_env();
        std::env::remove_var("MODEL");

        let provider = config.provider("anthropic").unwrap();
        assert!(matches!(provider.kind, ProviderKind::Anthropic));
        assert_eq!(provider.base_url, "https://api.anthropic.com");
        assert_eq!(provider.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn from_env_gemini_prefix_resolves_to_native_provider() {
        let _guard = crate::test_env_lock();
        std::env::set_var("MODEL", "gemini/gemini-2.5-flash");
        std::env::remove_var("MODEL_OPUS");
        std::env::remove_var("MODEL_SONNET");
        std::env::remove_var("MODEL_HAIKU");

        let config = ProviderConfig::from_env();
        std::env::remove_var("MODEL");

        let provider = config.provider("gemini").unwrap();
        assert!(matches!(provider.kind, ProviderKind::Gemini));
        assert_eq!(
            provider.base_url,
            "https://generativelanguage.googleapis.com/v1beta"
        );
    }

    #[test]
    fn from_env_cf_gateway_anthropic_builds_url_and_reuses_anthropic_kind() {
        let _guard = crate::test_env_lock();
        std::env::set_var("MODEL", "cf_gateway_anthropic/claude-sonnet-4-5");
        std::env::set_var("CF_AI_GATEWAY_ACCOUNT_ID", "acct123");
        std::env::set_var("CF_AI_GATEWAY_ID", "gw456");
        std::env::set_var("CF_AI_GATEWAY_TOKEN", "gwtoken");
        std::env::remove_var("MODEL_OPUS");
        std::env::remove_var("MODEL_SONNET");
        std::env::remove_var("MODEL_HAIKU");

        let config = ProviderConfig::from_env();
        std::env::remove_var("MODEL");
        std::env::remove_var("CF_AI_GATEWAY_ACCOUNT_ID");
        std::env::remove_var("CF_AI_GATEWAY_ID");
        std::env::remove_var("CF_AI_GATEWAY_TOKEN");

        let provider = config.provider("cf_gateway_anthropic").unwrap();
        assert!(matches!(provider.kind, ProviderKind::Anthropic));
        assert_eq!(
            provider.base_url,
            "https://gateway.ai.cloudflare.com/v1/acct123/gw456/anthropic"
        );
        assert_eq!(provider.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert!(provider
            .extra_headers
            .iter()
            .any(|(k, v)| k == "cf-aig-authorization" && v == "Bearer gwtoken"));
    }

    #[test]
    fn from_env_cf_gateway_openai_uses_compat_route() {
        let _guard = crate::test_env_lock();
        std::env::set_var("MODEL", "cf_gateway_openai/gpt-5");
        std::env::set_var("CF_AI_GATEWAY_ACCOUNT_ID", "acct123");
        std::env::set_var("CF_AI_GATEWAY_ID", "gw456");
        std::env::remove_var("CF_AI_GATEWAY_TOKEN");
        std::env::remove_var("MODEL_OPUS");
        std::env::remove_var("MODEL_SONNET");
        std::env::remove_var("MODEL_HAIKU");

        let config = ProviderConfig::from_env();
        std::env::remove_var("MODEL");
        std::env::remove_var("CF_AI_GATEWAY_ACCOUNT_ID");
        std::env::remove_var("CF_AI_GATEWAY_ID");

        let provider = config.provider("cf_gateway_openai").unwrap();
        assert!(matches!(provider.kind, ProviderKind::OpenAiCompatible));
        assert_eq!(
            provider.base_url,
            "https://gateway.ai.cloudflare.com/v1/acct123/gw456/compat"
        );
        assert!(provider.extra_headers.is_empty());
    }

    #[test]
    fn from_env_cf_gateway_missing_account_falls_back_to_default_free() {
        let _guard = crate::test_env_lock();
        std::env::set_var("MODEL", "cf_gateway_anthropic/claude-sonnet-4-5");
        std::env::remove_var("CF_AI_GATEWAY_ACCOUNT_ID");
        std::env::remove_var("CF_AI_GATEWAY_ID");
        std::env::remove_var("MODEL_OPUS");
        std::env::remove_var("MODEL_SONNET");
        std::env::remove_var("MODEL_HAIKU");

        let config = ProviderConfig::from_env();
        std::env::remove_var("MODEL");

        // No CF_AI_GATEWAY_ACCOUNT_ID/ID set -> can't build a base_url ->
        // falls back to default_free() rather than shipping a provider
        // entry with an unusable URL.
        assert!(config.provider("cf_gateway_anthropic").is_none());
        assert!(config.provider("nvidia-nim").is_some());
    }
}
