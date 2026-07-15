//! Compression prompt profiles. `Generic` is caveman's own embedded prompt
//! — a short string literal that has never had an external doc, so there's
//! no reason to invent a download mechanism for it (unlike ponytail's
//! SKILL.md, which lives in its own separately-maintained upstream repo).
//! `Custom` is used by callers that already have their own compression
//! spec text — e.g. short-skill reads its own `SKILL.md`'s "## Compression
//! spec" section and passes it here. This module has zero knowledge of
//! what a "skill" is.

pub enum Prompt {
    Generic,
    Custom(String),
}

const GENERIC_RULES: &str = "STRICT RULES:
- Do NOT modify anything inside ``` code blocks
- Do NOT modify anything inside inline backticks
- Preserve ALL URLs exactly
- Preserve ALL headings exactly
- Preserve file paths and commands
- Return ONLY the compressed markdown body — do NOT wrap the entire output in a ```markdown fence or any other fence. Inner code blocks from the original stay as-is; do not add a new outer fence around the whole file.

Only compress natural language.";

impl Prompt {
    #[must_use]
    pub fn build_compress_prompt(&self, body: &str) -> String {
        match self {
            Prompt::Generic => {
                format!(
                    "Compress this markdown into caveman format.\n\n{GENERIC_RULES}\n\nTEXT:\n{body}"
                )
            }
            Prompt::Custom(spec) => format!(
                "Compress this markdown body per the spec below.\n\n{spec}\n\nReturn ONLY the compressed markdown body — no outer fence, no explanation.\n\nBODY:\n{body}"
            ),
        }
    }

    #[must_use]
    pub fn build_fix_prompt(&self, original: &str, compressed: &str, errors: &[String]) -> String {
        let errors_str = errors
            .iter()
            .map(|e| format!("- {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "You are fixing a compressed markdown file. Specific validation errors were found.\n\n\
CRITICAL RULES:\n\
- DO NOT recompress or rephrase the file\n\
- ONLY fix the listed errors — leave everything else exactly as-is\n\
- The ORIGINAL is provided as reference only (to restore missing content)\n\
- Preserve compression style in all untouched sections\n\n\
ERRORS TO FIX:\n{errors_str}\n\n\
HOW TO FIX:\n\
- Missing URL: find it in ORIGINAL, restore it exactly where it belongs in COMPRESSED\n\
- Code block mismatch: find the exact code block in ORIGINAL, restore it in COMPRESSED\n\
- Heading mismatch: restore the exact heading text from ORIGINAL into COMPRESSED\n\
- Do not touch any section not mentioned in the errors\n\n\
ORIGINAL (reference only):\n{original}\n\n\
COMPRESSED (fix this):\n{compressed}\n\n\
Return ONLY the fixed compressed file. No explanation."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_prompt_contains_strict_rules_and_body() {
        let p = Prompt::Generic.build_compress_prompt("hello world");
        assert!(p.contains("STRICT RULES"));
        assert!(p.contains("hello world"));
    }

    #[test]
    fn custom_prompt_contains_spec_and_body() {
        let p = Prompt::Custom("MY SPEC TEXT".to_string()).build_compress_prompt("hello world");
        assert!(p.contains("MY SPEC TEXT"));
        assert!(p.contains("hello world"));
    }

    #[test]
    fn fix_prompt_lists_all_errors() {
        let errors = vec!["error one".to_string(), "error two".to_string()];
        let p = Prompt::Generic.build_fix_prompt("orig", "comp", &errors);
        assert!(p.contains("- error one"));
        assert!(p.contains("- error two"));
        assert!(p.contains("orig"));
        assert!(p.contains("comp"));
    }
}
