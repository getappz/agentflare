use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProviderKind {
    NvidiaNim,
    OpenRouter,
    LmStudio,
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

        let mut providers = Vec::new();
        for pt in &provider_types {
            let (base_url, api_key_env, kind) = match pt.as_str() {
                "nvidia_nim" => (
                    "https://integrate.api.nvidia.com/v1",
                    Some("NVIDIA_NIM_API_KEY"),
                    ProviderKind::NvidiaNim,
                ),
                "open_router" => (
                    "https://openrouter.ai/api/v1",
                    Some("OPENROUTER_API_KEY"),
                    ProviderKind::OpenRouter,
                ),
                "lmstudio" => (
                    "http://localhost:1234/v1",
                    None,
                    ProviderKind::LmStudio,
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

        for (keyword, opt) in [("opus", &model_opus), ("sonnet", &model_sonnet), ("haiku", &model_haiku)] {
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
                    kind: ProviderKind::NvidiaNim,
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
                },
                ProviderEntry {
                    id: "openrouter".into(),
                    kind: ProviderKind::OpenRouter,
                    base_url: "https://openrouter.ai/api/v1".into(),
                    api_key_env: Some("OPENROUTER_API_KEY".into()),
                    default_model: None,
                    models: vec![ModelDef {
                        id: "openrouter/auto".into(),
                        upstream_model: "openrouter/auto".into(),
                        max_input_tokens: None,
                    }],
                },
                ProviderEntry {
                    id: "lm-studio".into(),
                    kind: ProviderKind::LmStudio,
                    base_url: "http://localhost:1234/v1".into(),
                    api_key_env: None,
                    default_model: Some("local-model".into()),
                    models: vec![ModelDef {
                        id: "local-model".into(),
                        upstream_model: "local-model".into(),
                        max_input_tokens: Some(32_000),
                    }],
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

    pub fn route_for(&self, anthropic_model: &str) -> Option<&ModelRoute> {
        self.routing
            .iter()
            .find(|r| r.anthropic_model == anthropic_model)
    }

    pub fn provider(&self, id: &str) -> Option<&ProviderEntry> {
        self.providers.iter().find(|p| p.id == id)
    }
}
