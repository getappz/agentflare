//! Extracts usage examples from markdown documentation — the fenced code
//! blocks a package's own README/guides already carry.
//!
//! Deliberately not an LLM-generation step: every example returned here is
//! text the maintainer actually wrote, verbatim. That's the whole reason to
//! prefer this over a synthesized "how do I use this" snippet — it can't be
//! subtly wrong in a way the maintainer's own docs aren't.

/// Shortest code block worth keeping. Filters out noise like a bare
/// `npm install foo` one-liner, which isn't a usage example.
const MIN_EXAMPLE_CHARS: usize = 20;

/// Heading text substrings that mark a section as process/meta documentation
/// rather than usage — checked case-insensitively against the nearest
/// preceding heading. A code block under "Installation" or "Contributing"
/// is a real file in the README, just not an answer to "how do I use this".
const LOW_SIGNAL_HEADINGS: &[&str] = &[
    "install",
    "contribut",
    "develop",
    "clone",
    "build",
    "test",
    "release",
    "changelog",
    "license",
    "licence",
    "faq",
    "troubleshoot",
    "support",
    "citation",
    " cite",
    "acknowledg",
    "sponsor",
    "funding",
    "badge",
    "code of conduct",
    "security",
    "roadmap",
];

/// Shell commands that set up or maintain a project rather than use it —
/// checked against every line of a shell-flavored block. A block where
/// *every* line is one of these (a `$ git clone ...` / `$ pip install ...`
/// snippet) is setup, not a usage example, even under an otherwise-fine
/// heading like "Quick Start".
const SETUP_ONLY_PREFIXES: &[&str] = &[
    "git ",
    "pip ",
    "pip3 ",
    "python -m pip",
    "python3 -m pip",
    "npm ",
    "npx ",
    "yarn ",
    "pnpm ",
    "cd ",
    "curl ",
    "wget ",
    "make",
    "pytest",
    "tox",
    "cargo install",
    "brew ",
    "apt ",
    "apt-get ",
    "docker ",
];

fn is_low_signal_heading(title: &str) -> bool {
    let lower = title.to_ascii_lowercase();
    LOW_SIGNAL_HEADINGS.iter().any(|kw| lower.contains(kw))
}

/// A shell-flavored block (or an untagged one, since installs are often
/// fenced without a language) where every non-empty line is just a setup
/// command — no actual use of the package.
fn is_setup_only_shell(language: Option<&str>, code: &str) -> bool {
    let is_shell_like = matches!(
        language,
        None | Some("bash" | "sh" | "shell" | "console" | "zsh" | "text")
    );
    if !is_shell_like {
        return false;
    }
    code.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .all(|l| {
            let l = l.trim_start_matches('$').trim();
            SETUP_ONLY_PREFIXES.iter().any(|p| l.starts_with(p))
        })
}

/// One fenced code block recovered from markdown, paired with the nearest
/// preceding heading as a human-readable title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadmeExample {
    pub title: String,
    /// The fence's language tag (e.g. `js`, `python`), when present.
    pub language: Option<String>,
    pub code: String,
}

/// Extracts every fenced ``` code block from a markdown document.
///
/// Each block is titled with the nearest heading line above it, so multiple
/// examples under one "## Usage" section all share that title rather than
/// being indistinguishable. A document with no heading yet uses "Example".
pub fn extract_readme_examples(markdown: &str) -> Vec<ReadmeExample> {
    let mut out = Vec::new();
    let mut current_title = String::from("Example");
    let mut in_block = false;
    let mut language: Option<String> = None;
    let mut code_lines: Vec<&str> = Vec::new();

    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if let Some(fence) = trimmed.strip_prefix("```") {
            if in_block {
                let code = code_lines.join("\n");
                let long_enough = code.trim().chars().count() >= MIN_EXAMPLE_CHARS;
                if long_enough
                    && !is_low_signal_heading(&current_title)
                    && !is_setup_only_shell(language.as_deref(), &code)
                {
                    out.push(ReadmeExample {
                        title: current_title.clone(),
                        language: language.clone(),
                        code,
                    });
                }
                in_block = false;
                code_lines.clear();
            } else {
                in_block = true;
                let lang = fence.trim();
                language = if lang.is_empty() {
                    None
                } else {
                    Some(lang.to_string())
                };
            }
            continue;
        }
        if in_block {
            code_lines.push(line);
        } else if let Some(heading) = trimmed.strip_prefix('#') {
            current_title = heading.trim_start_matches('#').trim().to_string();
        }
    }
    // An unterminated fence (a truncated or malformed doc) yields whatever
    // came before it and drops the dangling block -- never panics.
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_a_titled_example() {
        let md = "# hono\n\n## Usage\n\n```ts\nimport { Hono } from 'hono'\nconst app = new Hono()\n```\n";
        let examples = extract_readme_examples(md);
        assert_eq!(examples.len(), 1);
        assert_eq!(examples[0].title, "Usage");
        assert_eq!(examples[0].language.as_deref(), Some("ts"));
        assert!(examples[0].code.contains("new Hono()"));
    }

    #[test]
    fn multiple_blocks_under_one_heading_share_its_title() {
        let md = "## Quickstart\n\n```js\nconst a = require('a')\n```\n\nSome prose.\n\n```js\nconst b = require('b')\n```\n";
        let examples = extract_readme_examples(md);
        assert_eq!(examples.len(), 2);
        assert!(examples.iter().all(|e| e.title == "Quickstart"));
    }

    #[test]
    fn a_block_before_any_heading_is_titled_example() {
        let md = "```js\nconst pkg = require('some-quite-long-package-name-here')\n```\n";
        let examples = extract_readme_examples(md);
        assert_eq!(examples[0].title, "Example");
    }

    #[test]
    fn trivial_one_liners_are_skipped() {
        let md = "```bash\nnpm i x\n```\n";
        assert!(extract_readme_examples(md).is_empty());
    }

    #[test]
    fn a_fence_with_no_language_tag_yields_none() {
        let md = "```\nplain code block long enough to keep\n```\n";
        let examples = extract_readme_examples(md);
        assert_eq!(examples[0].language, None);
    }

    #[test]
    fn an_unterminated_fence_does_not_panic_or_lose_earlier_blocks() {
        let md = "```js\nconst a = 1 + 1 + 1 + 1 + 1 + 1\n```\n\n```js\nconst b = unterminated";
        let examples = extract_readme_examples(md);
        assert_eq!(examples.len(), 1);
        assert!(examples[0].code.contains("const a"));
    }

    #[test]
    fn no_fences_yields_nothing() {
        assert!(extract_readme_examples("# Just prose\n\nNo code here.\n").is_empty());
    }

    #[test]
    fn setup_only_shell_blocks_are_skipped_even_under_a_good_heading() {
        // Verified live against requests' own PyPI long description: a
        // "Cloning the repository" section whose blocks are pure git/shell
        // plumbing, no actual use of the package.
        let md = "## Cloning the repository\n\n```bash\ngit clone -c fetch.fsck.badTimezone=ignore https://github.com/psf/requests.git\n```\n";
        assert!(extract_readme_examples(md).is_empty());
    }

    #[test]
    fn a_mixed_block_with_one_real_usage_line_is_kept() {
        // Every line must be setup for a block to be dropped -- one real
        // invocation line is enough to keep the whole block, since splitting
        // a fence mid-block isn't something this extractor does.
        let md = "## Usage\n\n```bash\ngit clone https://example.com/repo.git\nmytool run --config ./config.yaml\n```\n";
        let examples = extract_readme_examples(md);
        assert_eq!(
            examples.len(),
            1,
            "a block that isn't *purely* setup commands must survive"
        );
    }

    #[test]
    fn low_signal_headings_are_skipped_regardless_of_content() {
        for heading in [
            "## Installation",
            "## Contributing",
            "## License",
            "### Running tests",
        ] {
            let md = format!("{heading}\n\n```python\nimport sys\nprint(sys.version_info)\n```\n");
            assert!(
                extract_readme_examples(&md).is_empty(),
                "heading {heading:?} should have been treated as low-signal"
            );
        }
    }

    #[test]
    fn a_usage_heading_with_a_pip_install_line_keeps_only_the_real_example() {
        // The common README shape: "Quick Start" opens with an install
        // command before showing actual usage -- only the install block
        // should be dropped.
        let md = "## Quick Start\n\n```bash\n$ python -m pip install requests\n```\n\n```python\nimport requests\nr = requests.get('https://example.com')\n```\n";
        let examples = extract_readme_examples(md);
        assert_eq!(examples.len(), 1, "{examples:?}");
        assert!(examples[0].code.contains("requests.get"));
    }
}
