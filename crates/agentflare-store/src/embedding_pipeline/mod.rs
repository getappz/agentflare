pub mod model_registry;
pub mod download;
pub mod tokenizer;
pub mod pooling;

use std::path::{Path, PathBuf};

use model_registry::{EmbeddingModel, ModelConfig, VocabSource};
use tokenizer::{TokenizedInput, WordPieceTokenizer, HfTokenizerWrapper};

pub struct EmbeddingEngine {
    tokenizer: TokenizerKind,
    dimensions: usize,
    max_seq_len: usize,
    model_id: EmbeddingModel,
    model_config: ModelConfig,
    session: std::sync::Mutex<ort::session::Session>,
    input_names: InputNames,
    output_name: String,
}

enum TokenizerKind {
    WordPiece(WordPieceTokenizer),
    HfTokenizer(HfTokenizerWrapper),
}

struct InputNames {
    input_ids: String,
    attention_mask: String,
    token_type_ids: Option<String>,
}

fn is_bert_punctuation(ch: char) -> bool {
    if ch.is_ascii() {
        matches!(
            ch,
            '!' | '"' | '#' | '$' | '%' | '&' | '\'' | '(' | ')'
                | '*' | '+' | ',' | '-' | '.' | '/' | ':' | ';'
                | '<' | '=' | '>' | '?' | '@' | '[' | '\\' | ']'
                | '^' | '_' | '`' | '{' | '|' | '}' | '~'
        )
    } else {
        ch.is_ascii_punctuation()
    }
}

impl EmbeddingEngine {
    pub fn load(model_dir: &Path) -> anyhow::Result<Self> {
        let selected = model_registry::resolve_model();
        Self::load_model(model_dir, selected)
    }

    pub fn load_model(base_dir: &Path, model_id: EmbeddingModel) -> anyhow::Result<Self> {
        let config = model_id.config();
        let model_dir = base_dir.join(model_id.storage_dir_name());

        download::ensure_model(&model_dir, &config)?;

        let tokenizer = load_tokenizer(&model_dir, &config)?;
        let model_path = model_dir.join("model.onnx");

    let mut session = ort::session::Session::builder()
            .map_err(|e| anyhow::anyhow!("ORT builder: {e}"))?
            .with_intra_threads(
                std::thread::available_parallelism().map_or(4, |n| n.get().max(1)),
            )
            .map_err(|e| anyhow::anyhow!("ORT intra threads: {e}"))?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::All)
            .map_err(|e| anyhow::anyhow!("ORT optimization: {e}"))?
            .commit_from_file(&model_path)
            .map_err(|e| anyhow::anyhow!("ORT load model: {e}"))?;

        let input_names_list: Vec<String> = session
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .collect();

        let token_type_ids = if config.needs_token_type_ids {
            if input_names_list.len() < 3 {
                anyhow::bail!(
                    "Model {} requires token_type_ids but only has {} inputs",
                    config.name,
                    input_names_list.len()
                );
            }
            Some(input_names_list[2].clone())
        } else if input_names_list.len() >= 3 {
            Some(input_names_list[2].clone())
        } else {
            None
        };

        let output_name = session
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .ok_or_else(|| anyhow::anyhow!("Model has no named outputs"))?;

        let dimensions = detect_dimensions(
            &config,
            &tokenizer,
            &input_names_list,
            &token_type_ids,
            &output_name,
        )?;

        Ok(Self {
            session: std::sync::Mutex::new(session),
            tokenizer,
            dimensions,
            max_seq_len: config.max_seq_len,
            model_id,
            model_config: config,
            input_names: InputNames {
                input_ids: input_names_list[0].clone(),
                attention_mask: input_names_list[1].clone(),
                token_type_ids,
            },
            output_name,
        })
    }

    pub fn load_default() -> anyhow::Result<Self> {
        Self::load(&Self::model_directory())
    }

    pub fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let prefixed;
        let input_text = if let Some(prefix) = &self.model_config.document_prefix {
            prefixed = format!("{prefix}{text}");
            &prefixed
        } else {
            text
        };
        let input = tokenize(&self.tokenizer, input_text, self.max_seq_len);
        let mut hidden = self.run_inference(&input)?;

        let pooled = if let InputNames { token_type_ids: Some(_), .. } = &self.input_names {
            pooling::mean_pool(&hidden, &input.attention_mask, input.input_ids.len(), self.dimensions)
        } else {
            pooling::mean_pool(&hidden, &input.attention_mask, input.input_ids.len(), self.dimensions)
        };
        hidden = pooled;

        pooling::normalize_l2(&mut hidden);
        Ok(hidden)
    }

    pub fn embed_query(&self, query: &str) -> anyhow::Result<Vec<f32>> {
        let prefixed;
        let input_text = if let Some(prefix) = &self.model_config.query_prefix {
            prefixed = format!("{prefix}{query}");
            &prefixed
        } else {
            query
        };
        let input = tokenize(&self.tokenizer, input_text, self.max_seq_len);
        let mut hidden = self.run_inference(&input)?;

        let pooled = pooling::mean_pool(
            &hidden,
            &input.attention_mask,
            input.input_ids.len(),
            self.dimensions,
        );
        hidden = pooled;

        pooling::normalize_l2(&mut hidden);
        Ok(hidden)
    }

    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    pub fn model_name(&self) -> &str {
        &self.model_config.name
    }

    pub fn model_directory() -> PathBuf {
        if let Ok(dir) = std::env::var("AGENTFLARE_MODELS_DIR") {
            return PathBuf::from(dir);
        }
        let base = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("agentflare")
            .join("models");
        base
    }

    fn run_inference(&self, input: &TokenizedInput) -> anyhow::Result<Vec<f32>> {
        let seq_len = input.input_ids.len();
        let ids_vec: Vec<i64> = input.input_ids.iter().map(|&x| x as i64).collect();
        let mask_vec: Vec<i64> = input.attention_mask.iter().map(|&x| x as i64).collect();
        let ids_array = ndarray::Array2::from_shape_vec((1, seq_len), ids_vec)?;
        let mask_array = ndarray::Array2::from_shape_vec((1, seq_len), mask_vec)?;
        let ids_tensor = ort::value::Tensor::from_array(ids_array)?;
        let mask_tensor = ort::value::Tensor::from_array(mask_array)?;

        let mut session = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(type_id) = &self.input_names.token_type_ids {
            let type_vec: Vec<i64> = input.token_type_ids.iter().map(|&x| x as i64).collect();
            let type_array = ndarray::Array2::from_shape_vec((1, seq_len), type_vec)?;
            let type_tensor = ort::value::Tensor::from_array(type_array)?;
            let outputs = session.run(ort::inputs![
                self.input_names.input_ids.as_str() => ids_tensor,
                self.input_names.attention_mask.as_str() => mask_tensor,
                type_id.as_str() => type_tensor,
            ])?;
            let (_, data) = outputs[self.output_name.as_str()].try_extract_tensor::<f32>()?;
            Ok(data.to_vec())
        } else {
            let outputs = session.run(ort::inputs![
                self.input_names.input_ids.as_str() => ids_tensor,
                self.input_names.attention_mask.as_str() => mask_tensor,
            ])?;
            let (_, data) = outputs[self.output_name.as_str()].try_extract_tensor::<f32>()?;
            Ok(data.to_vec())
        }
    }
}

fn load_tokenizer(model_dir: &Path, config: &ModelConfig) -> anyhow::Result<TokenizerKind> {
    match &config.vocab_file {
        VocabSource::VocabTxt(filename) => {
            let path = model_dir.join(filename);
            let tok = WordPieceTokenizer::from_file(&path)?;
            Ok(TokenizerKind::WordPiece(tok))
        }
        VocabSource::TokenizerJson(filename) => {
            let path = model_dir.join(filename);
            let tok = HfTokenizerWrapper::from_file(&path)?;
            Ok(TokenizerKind::HfTokenizer(tok))
        }
    }
}

fn tokenize(tokenizer: &TokenizerKind, text: &str, max_len: usize) -> TokenizedInput {
    match tokenizer {
        TokenizerKind::WordPiece(wp) => wp.encode(text, max_len),
        TokenizerKind::HfTokenizer(hf) => hf.encode(text, max_len),
    }
}

fn detect_dimensions(
    config: &ModelConfig,
    tokenizer: &TokenizerKind,
    input_names: &[String],
    token_type_ids: &Option<String>,
    output_name: &str,
) -> anyhow::Result<usize> {
    let dummy = tokenize(tokenizer, "test", 8);
    let seq_len = dummy.input_ids.len();
    if seq_len == 0 {
        return Ok(config.dimensions);
    }

    let ids_vec: Vec<i64> = dummy.input_ids.iter().map(|&x| x as i64).collect();
    let mask_vec: Vec<i64> = dummy.attention_mask.iter().map(|&x| x as i64).collect();
    let ids_array = ndarray::Array2::from_shape_vec((1, seq_len), ids_vec)?;
    let mask_array = ndarray::Array2::from_shape_vec((1, seq_len), mask_vec)?;
    let ids_tensor = ort::value::Tensor::from_array(ids_array)?;
    let mask_tensor = ort::value::Tensor::from_array(mask_array)?;

    let mut session = ort::session::Session::builder()
        .map_err(|e| anyhow::anyhow!("ORT builder: {e}"))?
        .with_intra_threads(1)
        .map_err(|e| anyhow::anyhow!("ORT intra threads: {e}"))?
        .commit_from_file(&config.model_url())
        .map_err(|_| anyhow::anyhow!("cannot probe dimensions without model file"))?;

    let outputs = if let Some(type_id) = token_type_ids {
        let type_vec: Vec<i64> = dummy.token_type_ids.iter().map(|&x| x as i64).collect();
        let type_array = ndarray::Array2::from_shape_vec((1, seq_len), type_vec)?;
        let type_tensor = ort::value::Tensor::from_array(type_array)?;
        session.run(ort::inputs![
            input_names[0].as_str() => ids_tensor,
            input_names[1].as_str() => mask_tensor,
            type_id.as_str() => type_tensor,
        ])?
    } else {
        session.run(ort::inputs![
            input_names[0].as_str() => ids_tensor,
            input_names[1].as_str() => mask_tensor,
        ])?
    };

    let (shape, _) = outputs[output_name].try_extract_tensor::<f32>()?;
    shape.last().copied().map(|s| s as usize).ok_or_else(|| {
        anyhow::anyhow!("could not detect embedding dimensions from model output")
    })
}
