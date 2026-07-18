use std::collections::HashMap;
use std::path::Path;

pub struct WordPieceTokenizer {
    vocab: HashMap<String, i32>,
    pub cls_id: i32,
    pub sep_id: i32,
    pub pad_id: i32,
    pub unk_id: i32,
    max_word_chars: usize,
}

#[derive(Debug, Clone)]
pub struct TokenizedInput {
    pub input_ids: Vec<i32>,
    pub attention_mask: Vec<i32>,
    pub token_type_ids: Vec<i32>,
}

impl WordPieceTokenizer {
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read vocab file {}: {}", path.display(), e))?;
        Self::from_vocab_str(&content)
    }

    pub fn from_vocab_str(vocab_str: &str) -> anyhow::Result<Self> {
        let vocab: HashMap<String, i32> = vocab_str
            .lines()
            .enumerate()
            .map(|(i, line)| (line.to_string(), i as i32))
            .collect();

        let cls_id = *vocab
            .get("[CLS]")
            .ok_or_else(|| anyhow::anyhow!("Vocabulary missing [CLS] token"))?;
        let sep_id = *vocab
            .get("[SEP]")
            .ok_or_else(|| anyhow::anyhow!("Vocabulary missing [SEP] token"))?;
        let pad_id = *vocab
            .get("[PAD]")
            .ok_or_else(|| anyhow::anyhow!("Vocabulary missing [PAD] token"))?;
        let unk_id = *vocab
            .get("[UNK]")
            .ok_or_else(|| anyhow::anyhow!("Vocabulary missing [UNK] token"))?;

        Ok(Self {
            vocab,
            cls_id,
            sep_id,
            pad_id,
            unk_id,
            max_word_chars: 200,
        })
    }

    pub fn encode(&self, text: &str, max_len: usize) -> TokenizedInput {
        let words = self.pre_tokenize(text);
        let mut ids = vec![self.cls_id];

        for word in &words {
            if ids.len() >= max_len - 1 {
                break;
            }
            let subword_ids = self.wordpiece_encode(word);
            for id in subword_ids {
                if ids.len() >= max_len - 1 {
                    break;
                }
                ids.push(id);
            }
        }
        ids.push(self.sep_id);

        let len = ids.len();
        TokenizedInput {
            input_ids: ids,
            attention_mask: vec![1; len],
            token_type_ids: vec![0; len],
        }
    }

    fn pre_tokenize(&self, text: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut current = String::new();

        for ch in text.chars() {
            if ch.is_whitespace() {
                if !current.is_empty() {
                    words.extend(self.split_identifier(&current));
                    current.clear();
                }
            } else if is_bert_punctuation(ch) {
                if !current.is_empty() {
                    words.extend(self.split_identifier(&current));
                    current.clear();
                }
                words.push(ch.to_string());
            } else {
                current.push(ch);
            }
        }
        if !current.is_empty() {
            words.extend(self.split_identifier(&current));
        }
        words.iter().map(|w| w.to_lowercase()).collect()
    }

    fn split_identifier(&self, word: &str) -> Vec<String> {
        let lower = word.to_lowercase();
        if self.vocab.contains_key(&lower) {
            return vec![word.to_string()];
        }
        let mut parts = Vec::new();
        let mut current = String::new();
        let chars: Vec<char> = word.chars().collect();
        for (i, &ch) in chars.iter().enumerate() {
            if ch == '_' || ch == '-' {
                if !current.is_empty() {
                    parts.push(current.clone());
                    current.clear();
                }
            } else if i > 0 && ch.is_ascii_uppercase() && chars[i - 1].is_ascii_lowercase() {
                if !current.is_empty() {
                    parts.push(current.clone());
                    current.clear();
                }
                current.push(ch);
            } else {
                current.push(ch);
            }
        }
        if !current.is_empty() {
            parts.push(current);
        }
        if parts.is_empty() {
            vec![word.to_string()]
        } else {
            parts
        }
    }

    fn wordpiece_encode(&self, word: &str) -> Vec<i32> {
        if word.chars().count() > self.max_word_chars {
            return vec![self.unk_id];
        }
        let chars: Vec<char> = word.chars().collect();
        let mut tokens = Vec::new();
        let mut start = 0;
        while start < chars.len() {
            let mut end = chars.len();
            let mut matched = false;
            while start < end {
                let substr: String = chars[start..end].iter().collect();
                let candidate = if start > 0 {
                    format!("##{substr}")
                } else {
                    substr
                };
                if let Some(&id) = self.vocab.get(&candidate) {
                    tokens.push(id);
                    matched = true;
                    start = end;
                    break;
                }
                end -= 1;
            }
            if !matched {
                // Standard WordPiece: a word that can't be fully segmented
                // maps to a single [UNK], not one UNK per unmatched
                // character interleaved with whatever subwords did match.
                return vec![self.unk_id];
            }
        }
        tokens
    }
}

pub struct BpeTokenizer {
    vocab: HashMap<String, i32>,
    ranks: HashMap<(String, String), usize>,
    unk_id: i32,
    lowercase: bool,
}

impl BpeTokenizer {
    fn from_json(model: &serde_json::Value) -> anyhow::Result<Self> {
        let vocab_obj = model
            .get("vocab")
            .and_then(|v| v.as_object())
            .ok_or_else(|| anyhow::anyhow!("tokenizer.json BPE model missing vocab object"))?;

        let mut vocab = HashMap::new();
        for (token, id) in vocab_obj {
            if let Some(id) = id.as_i64() {
                vocab.insert(token.clone(), id as i32);
            }
        }

        let unk_id = *vocab
            .get("<unk>")
            .or_else(|| vocab.get("<UNK>"))
            .or_else(|| vocab.get(""))
            .ok_or_else(|| anyhow::anyhow!("BPE vocab missing <unk> token"))?;

        let merges = model
            .get("merges")
            .and_then(|m| m.as_array())
            .ok_or_else(|| anyhow::anyhow!("tokenizer.json BPE model missing merges"))?;

        let mut ranks = HashMap::new();
        for (i, m) in merges.iter().enumerate() {
            let pair = match m {
                serde_json::Value::String(s) => {
                    let mut parts = s.splitn(2, ' ');
                    let left = parts.next().unwrap_or("").to_string();
                    let right = parts.next().unwrap_or("").to_string();
                    (left, right)
                }
                serde_json::Value::Array(arr) if arr.len() == 2 => {
                    let left = arr[0].as_str().unwrap_or("").to_string();
                    let right = arr[1].as_str().unwrap_or("").to_string();
                    (left, right)
                }
                _ => anyhow::bail!("BPE merge entry malformed: {m}"),
            };
            ranks.insert(pair, i);
        }

        let lowercase = model
            .get("lowercase")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Ok(Self {
            vocab,
            ranks,
            unk_id,
            lowercase,
        })
    }

    pub fn encode(&self, text: &str, max_len: usize) -> TokenizedInput {
        let lowered: String;
        let text = if self.lowercase {
            lowered = text.to_lowercase();
            lowered.as_str()
        } else {
            text
        };

        let mut ids = Vec::new();
        for word in text.split_whitespace() {
            self.bpe_encode_word(word, &mut ids, max_len);
            if ids.len() >= max_len {
                break;
            }
        }

        let len = ids.len();
        TokenizedInput {
            input_ids: ids,
            attention_mask: vec![1; len],
            token_type_ids: vec![0; len],
        }
    }

    fn bpe_encode_word(&self, word: &str, ids: &mut Vec<i32>, max_len: usize) {
        if word.is_empty() {
            return;
        }
        let mut symbols: Vec<String> = word.chars().map(|c| c.to_string()).collect();

        loop {
            if symbols.len() < 2 {
                break;
            }
            let mut best_rank: Option<usize> = None;
            let mut best_idx: Option<usize> = None;
            for i in 0..symbols.len() - 1 {
                if let Some(&r) = self
                    .ranks
                    .get(&(symbols[i].clone(), symbols[i + 1].clone()))
                    && best_rank.is_none_or(|br| r < br)
                {
                    best_rank = Some(r);
                    best_idx = Some(i);
                }
            }
            match best_idx {
                Some(i) => {
                    let merged = symbols[i].clone() + &symbols[i + 1];
                    symbols.remove(i + 1);
                    symbols[i] = merged;
                }
                None => break,
            }
        }

        for sym in symbols {
            if ids.len() >= max_len {
                break;
            }
            let id = self.vocab.get(&sym).copied().unwrap_or(self.unk_id);
            ids.push(id);
        }
    }
}

pub enum HfTokenizerInner {
    WordPiece(WordPieceTokenizer),
    Bpe(BpeTokenizer),
}

/// Parses only `model.type`/`model.vocab`/`model.merges` from a HF
/// `tokenizer.json`; pre-tokenization always runs this crate's own
/// whitespace/BERT-punctuation splitter + lowercasing (see
/// [`WordPieceTokenizer::pre_tokenize`]), not the file's own `normalizer`,
/// `pre_tokenizer`, or `post_processor` sections (NFD/accent-stripping,
/// byte-level/Metaspace pre-tokenizers, template special-token insertion,
/// etc). This matches the built-in MiniLM/Nomic models, which use plain
/// BERT-style WordPiece, but a custom `hf:owner/repo` model that relies on a
/// non-default normalizer or pre-tokenizer will tokenize differently than
/// the reference HF implementation. Full parity would mean implementing the
/// whole tokenizers normalizer/pre-tokenizer/post-processor grammar, which
/// is out of scope here — known limitation, not a bug in the common path.
pub struct HfTokenizerWrapper {
    inner: HfTokenizerInner,
}

impl HfTokenizerWrapper {
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::from_json(&content)
    }

    fn from_json(json_str: &str) -> anyhow::Result<Self> {
        let parsed: serde_json::Value = serde_json::from_str(json_str)?;
        let model = parsed
            .get("model")
            .ok_or_else(|| anyhow::anyhow!("tokenizer.json missing model"))?;
        let model_type = model
            .get("type")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow::anyhow!("tokenizer.json model missing type"))?;

        let inner = match model_type {
            "WordPiece" => {
                let vocab_obj = model
                    .get("vocab")
                    .and_then(|v| v.as_object())
                    .ok_or_else(|| anyhow::anyhow!("tokenizer.json missing model.vocab object"))?;

                let mut vocab_lines: Vec<(String, i32)> = vocab_obj
                    .iter()
                    .filter_map(|(token, id)| id.as_i64().map(|id| (token.clone(), id as i32)))
                    .collect();
                vocab_lines.sort_by_key(|(_, id)| *id);

                for (token, _) in &mut vocab_lines {
                    let mapped = match token.as_str() {
                        "<s>" => "[CLS]",
                        "</s>" => "[SEP]",
                        "<pad>" => "[PAD]",
                        "<unk>" => "[UNK]",
                        "<mask>" => "[MASK]",
                        _ => continue,
                    };
                    *token = mapped.to_string();
                }

                let vocab_str: String = vocab_lines
                    .into_iter()
                    .map(|(token, _)| token)
                    .collect::<Vec<_>>()
                    .join("\n");

                let wp = WordPieceTokenizer::from_vocab_str(&vocab_str)?;
                HfTokenizerInner::WordPiece(wp)
            }
            "BPE" => {
                let bpe = BpeTokenizer::from_json(model)?;
                HfTokenizerInner::Bpe(bpe)
            }
            other => anyhow::bail!(
                "Unsupported tokenizer model.type: {other}. Only WordPiece and BPE are supported."
            ),
        };

        Ok(Self { inner })
    }

    pub fn encode(&self, text: &str, max_len: usize) -> TokenizedInput {
        match &self.inner {
            HfTokenizerInner::WordPiece(wp) => wp.encode(text, max_len),
            HfTokenizerInner::Bpe(bpe) => bpe.encode(text, max_len),
        }
    }
}

fn is_bert_punctuation(ch: char) -> bool {
    if ch.is_ascii() {
        matches!(
            ch,
            '!' | '"'
                | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '('
                | ')'
                | '*'
                | '+'
                | ','
                | '-'
                | '.'
                | '/'
                | ':'
                | ';'
                | '<'
                | '='
                | '>'
                | '?'
                | '@'
                | '['
                | '\\'
                | ']'
                | '^'
                | '_'
                | '`'
                | '{'
                | '|'
                | '}'
                | '~'
        )
    } else {
        ch.is_ascii_punctuation()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BPE_JSON: &str = r#"{
      "model": {
        "type": "BPE",
        "vocab": {
          "<unk>": 0,
          "h": 1, "e": 2, "l": 3, "o": 4,
          "he": 5, "hel": 6, "hell": 7, "hello": 8
        },
        "merges": ["h e", "he l", "hel l", "hell o"],
        "unk_token": "<unk>",
        "lowercase": true
      }
    }"#;

    const WORDPICE_JSON: &str = r###"{
      "model": {
        "type": "WordPiece",
        "vocab": {
          "[PAD]": 0, "[UNK]": 1, "[CLS]": 2, "[SEP]": 3,
          "hello": 4, "##llo": 5, "world": 6
        }
      }
    }"###;

    #[test]
    fn bpe_applies_merge_rules() {
        let tok = HfTokenizerWrapper::from_json(BPE_JSON).unwrap();
        let out = tok.encode("hello", 32);
        assert_eq!(out.input_ids, vec![8]);
    }

    #[test]
    fn bpe_tokenizes_multiple_words_and_unk() {
        let tok = HfTokenizerWrapper::from_json(BPE_JSON).unwrap();
        let out = tok.encode("hello hello", 32);
        assert_eq!(out.input_ids, vec![8, 8]);

        let out = tok.encode("helloz", 32);
        assert_eq!(out.input_ids, vec![8, 0]);
    }

    #[test]
    fn wordpiece_path_still_works() {
        let tok = HfTokenizerWrapper::from_json(WORDPICE_JSON).unwrap();
        let out = tok.encode("hello", 32);
        // [CLS]=2, hello=4, [SEP]=3
        assert_eq!(out.input_ids, vec![2, 4, 3]);
    }

    #[test]
    fn wordpiece_unsegmentable_word_is_single_unk() {
        let tok = HfTokenizerWrapper::from_json(WORDPICE_JSON).unwrap();
        // "helloz" greedily matches "hello" then fails on the trailing "z"
        // (no "##z" in vocab) — the whole word should collapse to one
        // [UNK], not "hello" followed by a stray [UNK].
        let out = tok.encode("helloz", 32);
        // [CLS]=2, [UNK]=1, [SEP]=3
        assert_eq!(out.input_ids, vec![2, 1, 3]);
    }

    #[test]
    fn unknown_model_type_fails_loudly() {
        let json = r#"{ "model": { "type": "Unigram", "vocab": {} } }"#;
        let err = HfTokenizerWrapper::from_json(json);
        let msg = match err {
            Ok(_) => panic!("expected Err for unsupported model.type"),
            Err(e) => e.to_string(),
        };
        assert!(msg.contains("Unsupported tokenizer"));
    }

    #[test]
    fn missing_model_type_fails() {
        let json = r#"{ "model": { "vocab": {} } }"#;
        assert!(HfTokenizerWrapper::from_json(json).is_err());
    }
}
