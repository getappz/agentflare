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

        let cls_id = *vocab.get("[CLS]")
            .ok_or_else(|| anyhow::anyhow!("Vocabulary missing [CLS] token"))?;
        let sep_id = *vocab.get("[SEP]")
            .ok_or_else(|| anyhow::anyhow!("Vocabulary missing [SEP] token"))?;
        let pad_id = *vocab.get("[PAD]")
            .ok_or_else(|| anyhow::anyhow!("Vocabulary missing [PAD] token"))?;
        let unk_id = *vocab.get("[UNK]")
            .ok_or_else(|| anyhow::anyhow!("Vocabulary missing [UNK] token"))?;

        Ok(Self { vocab, cls_id, sep_id, pad_id, unk_id, max_word_chars: 200 })
    }

    pub fn encode(&self, text: &str, max_len: usize) -> TokenizedInput {
        let words = self.pre_tokenize(text);
        let mut ids = vec![self.cls_id];

        for word in &words {
            if ids.len() >= max_len - 1 { break; }
            let subword_ids = self.wordpiece_encode(word);
            for id in subword_ids {
                if ids.len() >= max_len - 1 { break; }
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
                if !current.is_empty() { parts.push(current.clone()); current.clear(); }
            } else if i > 0 && ch.is_ascii_uppercase() && chars[i - 1].is_ascii_lowercase() {
                if !current.is_empty() { parts.push(current.clone()); current.clear(); }
                current.push(ch);
            } else {
                current.push(ch);
            }
        }
        if !current.is_empty() { parts.push(current); }
        if parts.is_empty() { vec![word.to_string()] } else { parts }
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
                let candidate = if start > 0 { format!("##{substr}") } else { substr };
                if let Some(&id) = self.vocab.get(&candidate) {
                    tokens.push(id);
                    matched = true;
                    start = end;
                    break;
                }
                end -= 1;
            }
            if !matched {
                tokens.push(self.unk_id);
                start += 1;
            }
        }
        tokens
    }
}

pub struct HfTokenizerWrapper {
    inner: WordPieceTokenizer,
}

impl HfTokenizerWrapper {
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::from_json(&content)
    }

    fn from_json(json_str: &str) -> anyhow::Result<Self> {
        let parsed: serde_json::Value = serde_json::from_str(json_str)?;
        let vocab_obj = parsed
            .get("model")
            .and_then(|m| m.get("vocab"))
            .and_then(|v| v.as_object())
            .ok_or_else(|| anyhow::anyhow!("tokenizer.json missing model.vocab object"))?;

        let mut vocab_lines: Vec<(String, i32)> = vocab_obj
            .iter()
            .filter_map(|(token, id)| id.as_i64().map(|id| (token.clone(), id as i32)))
            .collect();
        vocab_lines.sort_by_key(|(_, id)| *id);

        for (token, _) in &mut vocab_lines {
            let mapped = match token.as_str() {
                "<s>" => "[CLS]", "</s>" => "[SEP]",
                "<pad>" => "[PAD]", "<unk>" => "[UNK]",
                "<mask>" => "[MASK]", _ => continue,
            };
            *token = mapped.to_string();
        }

        let vocab_str: String = vocab_lines.into_iter()
            .map(|(token, _)| token)
            .collect::<Vec<_>>()
            .join("\n");

        let inner = WordPieceTokenizer::from_vocab_str(&vocab_str)?;
        Ok(Self { inner })
    }

    pub fn encode(&self, text: &str, max_len: usize) -> TokenizedInput {
        self.inner.encode(text, max_len)
    }
}

fn is_bert_punctuation(ch: char) -> bool {
    if ch.is_ascii() {
        matches!(ch,
            '!' | '"' | '#' | '$' | '%' | '&' | '\'' | '(' | ')'
            | '*' | '+' | ',' | '-' | '.' | '/' | ':' | ';'
            | '<' | '=' | '>' | '?' | '@' | '[' | '\\' | ']'
            | '^' | '_' | '`' | '{' | '|' | '}' | '~'
        )
    } else {
        ch.is_ascii_punctuation()
    }
}
