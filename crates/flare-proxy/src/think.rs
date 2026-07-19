/// Strip `<think>...</think>` blocks from assistant text, returning
/// (cleaned_text, thinking_content).
pub fn strip_think_tags(text: &str) -> (String, Vec<String>) {
    let mut cleaned = String::with_capacity(text.len());
    let mut thoughts = Vec::new();
    let mut rest = text;

    while let Some(start) = rest.find("<think>") {
        cleaned.push_str(&rest[..start]);
        rest = &rest[start + 7..];
        if let Some(end) = rest.find("</think>") {
            thoughts.push(rest[..end].to_string());
            rest = &rest[end + 8..];
        } else {
            cleaned.push_str("<think>");
            cleaned.push_str(rest);
            rest = "";
            break;
        }
    }
    cleaned.push_str(rest);

    (cleaned, thoughts)
}

/// Check if output needs think-tag parsing (free-tier models sometimes emit them).
pub fn needs_think_parsing(model: &str) -> bool {
    let model = model.to_lowercase();
    model.contains("deepseek")
        || model.contains("qwen")
        || model.contains("llama")
        || model.contains("mistral")
        || model.contains("mixtral")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strips_simple_think() {
        let (text, thoughts) = strip_think_tags("Hello <think>let me think</think> world");
        assert_eq!(text, "Hello  world");
        assert_eq!(thoughts, vec!["let me think"]);
    }

    #[test]
    fn test_no_think_tags() {
        let (text, thoughts) = strip_think_tags("Hello world");
        assert_eq!(text, "Hello world");
        assert!(thoughts.is_empty());
    }

    #[test]
    fn test_unclosed_think_tag() {
        let (text, thoughts) = strip_think_tags("Hello <think>unclosed");
        assert_eq!(text, "Hello <think>unclosed");
        assert!(thoughts.is_empty());
    }
}
