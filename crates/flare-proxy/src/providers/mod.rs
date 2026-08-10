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
            "nvidia_nim" | "open_router" | "lmstudio" | "anthropic" | "gemini"
        )
    })
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
                "agentflare: unsupported provider prefix '{bad}' in MODEL/MODEL_OPUS/MODEL_SONNET/MODEL_HAIKU -- falling back to default_free() config. Supported prefixes: nvidia_nim, open_router, lmstudio, anthropic, gemini."
            );
            return Self::default_free();
        }

        let mut providers = Vec::new();
        for pt in &provider_types {
            let (base_url, api_key_env, kind, extra_headers) = match pt.as_str() {
                "nvidia_nim" => (
                    "https://integrate.api.nvidia.com/v1",
                    Some("NVIDIA_NIM_API_KEY"),
                    ProviderKind::OpenAiCompatible,
                    vec![],
                ),
                "open_router" => (
                    "https://openrouter.ai/api/v1",
                    Some("OPENROUTER_API_KEY"),
                    ProviderKind::OpenAiCompatible,
                    openrouter_headers(),
                ),
                "lmstudio" => (
                    "http://localhost:1234/v1",
                    None,
                    ProviderKind::OpenAiCompatible,
                    vec![],
                ),
                "anthropic" => (
                    "https://api.anthropic.com",
                    Some("ANTHROPIC_API_KEY"),
                    ProviderKind::Anthropic,
                    vec![],
                ),
                "gemini" => (
                    "https://generativelanguage.googleapis.com/v1beta",
                    Some("GEMINI_API_KEY"),
                    ProviderKind::Gemini,
                    vec![],
                ),
                _ => continue,
            };

            providers.push(ProviderEntry {
                id: pt.clone(),
                kind,
                base_url: base_url.into(),
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
}
