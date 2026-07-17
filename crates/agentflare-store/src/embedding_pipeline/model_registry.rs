use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddingModel {
    AllMiniLmL6V2,
    NomicEmbedV1_5,
    Custom(CustomModelSpec),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct CustomModelSpec {
    pub repo: String,
    pub revision: Option<String>,
    pub dimensions: Option<usize>,
}

impl CustomModelSpec {
    fn parse(s: &str) -> Option<Self> {
        let (repo, revision) = match s.split_once('@') {
            Some((r, rev)) => (
                r.trim(),
                Some(rev.trim().to_string()).filter(|v| !v.is_empty()),
            ),
            None => (s.trim(), None),
        };
        let mut parts = repo.split('/');
        let (owner, name) = (parts.next()?, parts.next()?);
        if parts.next().is_some() || owner.is_empty() || name.is_empty()
            || repo.chars().any(char::is_whitespace)
        {
            return None;
        }
        Some(Self { repo: repo.to_string(), revision, dimensions: None })
    }

    fn storage_slug(&self) -> String {
        let mut slug = String::from("hf-");
        for c in self.repo.chars() {
            slug.push(match c {
                'a'..='z' | '0'..='9' | '-' => c,
                'A'..='Z' => c.to_ascii_lowercase(),
                _ => '-',
            });
        }
        if let Some(rev) = &self.revision {
            slug.push('-');
            for c in rev.chars().take(16) {
                slug.push(match c {
                    'a'..='z' | '0'..='9' | '-' => c,
                    'A'..='Z' => c.to_ascii_lowercase(),
                    _ => '-',
                });
            }
        }
        slug
    }
}

impl EmbeddingModel {
    pub const DEFAULT: Self = Self::AllMiniLmL6V2;

    pub fn config(&self) -> ModelConfig {
        match self {
            Self::AllMiniLmL6V2 => ModelConfig {
                model: self.clone(),
                name: "all-MiniLM-L6-v2".into(),
                hf_repo: "sentence-transformers/all-MiniLM-L6-v2".into(),
                revision: None,
                onnx_path: "onnx/model.onnx".into(),
                vocab_file: VocabSource::VocabTxt("vocab.txt".into()),
                dimensions: 384,
                max_seq_len: 256,
                model_min_bytes: 1_000_000,
                vocab_min_bytes: 100_000,
                query_prefix: None,
                document_prefix: None,
                needs_token_type_ids: true,
            },
            Self::NomicEmbedV1_5 => ModelConfig {
                model: self.clone(),
                name: "nomic-embed-text-v1.5".into(),
                hf_repo: "nomic-ai/nomic-embed-text-v1.5".into(),
                revision: None,
                onnx_path: "onnx/model.onnx".into(),
                vocab_file: VocabSource::VocabTxt("vocab.txt".into()),
                dimensions: 768,
                max_seq_len: 512,
                model_min_bytes: 100_000_000,
                vocab_min_bytes: 100_000,
                query_prefix: Some("search_query: ".into()),
                document_prefix: Some("search_document: ".into()),
                needs_token_type_ids: false,
            },
            Self::Custom(spec) => ModelConfig {
                model: self.clone(),
                name: match &spec.revision {
                    Some(rev) => format!("hf:{}@{rev}", spec.repo),
                    None => format!("hf:{}", spec.repo),
                },
                hf_repo: spec.repo.clone(),
                revision: spec.revision.clone(),
                onnx_path: "onnx/model.onnx".into(),
                vocab_file: VocabSource::TokenizerJson("tokenizer.json".into()),
                dimensions: spec.dimensions.unwrap_or(768),
                max_seq_len: 512,
                model_min_bytes: 1_000_000,
                vocab_min_bytes: 1_000,
                query_prefix: None,
                document_prefix: None,
                needs_token_type_ids: false,
            },
        }
    }

    pub fn from_str_name(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        if let Some(rest) = trimmed.strip_prefix("hf:") {
            return CustomModelSpec::parse(rest).map(Self::Custom);
        }
        match trimmed.to_lowercase().replace('_', "-").as_str() {
            "all-minilm-l6-v2" | "minilm" | "default" => Some(Self::AllMiniLmL6V2),
            "nomic-embed-v1.5" | "nomic-embed-text-v1.5" | "nomic" | "nomic-embed" => {
                Some(Self::NomicEmbedV1_5)
            }
            _ => None,
        }
    }

    pub const ALL: &'static [Self] = &[Self::AllMiniLmL6V2, Self::NomicEmbedV1_5];

    pub fn storage_dir_name(&self) -> String {
        match self {
            Self::AllMiniLmL6V2 => "all-minilm-l6-v2".to_string(),
            Self::NomicEmbedV1_5 => "nomic-embed-v1.5".to_string(),
            Self::Custom(spec) => spec.storage_slug(),
        }
    }
}

impl fmt::Display for EmbeddingModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.config().name)
    }
}

#[derive(Debug, Clone)]
pub enum VocabSource {
    VocabTxt(String),
    TokenizerJson(String),
}

impl VocabSource {
    pub fn filename(&self) -> &str {
        match self {
            Self::VocabTxt(f) | Self::TokenizerJson(f) => f,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub model: EmbeddingModel,
    pub name: String,
    pub hf_repo: String,
    pub revision: Option<String>,
    pub onnx_path: String,
    pub vocab_file: VocabSource,
    pub dimensions: usize,
    pub max_seq_len: usize,
    pub model_min_bytes: u64,
    pub vocab_min_bytes: u64,
    pub query_prefix: Option<String>,
    pub document_prefix: Option<String>,
    pub needs_token_type_ids: bool,
}

impl ModelConfig {
    fn resolve_base(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}",
            self.hf_repo,
            self.revision.as_deref().unwrap_or("main")
        )
    }

    pub fn model_url(&self) -> String {
        format!("{}/{}", self.resolve_base(), self.onnx_path)
    }

    pub fn vocab_url(&self) -> String {
        format!("{}/{}", self.resolve_base(), self.vocab_file.filename())
    }
}

pub fn resolve_model() -> EmbeddingModel {
    let env_val = std::env::var("AGENTFLARE_EMBEDDING_MODEL").ok();
    let name = env_val.as_deref().unwrap_or("default");
    EmbeddingModel::from_str_name(name).unwrap_or(EmbeddingModel::DEFAULT)
}
