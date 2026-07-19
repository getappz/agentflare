use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub providers: Vec<ProviderEntry>,
    pub routing: Vec<ModelRoute>,
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

impl ProviderConfig {
    pub fn default_free() -> Self {
        Self {
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
