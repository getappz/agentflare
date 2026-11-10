# Configurable summarize output

User request: add the reference skill features discussed in chat: configurable
summaries, text/Markdown/JSON output, and timestamped transcript extraction.

The CLI will summarize by default, reusing the existing Anthropic API / Claude
CLI model path. `--extract` preserves caption-only use without a model call.
The bundled agent skill will explicitly use `--extract --json`, allowing the
calling agent to continue writing the summary. Caption language selection stays
on `--language`; `--summary-language` controls the generated prose.

- [x] Add tests and implementation for structured summaries, named lengths,
  custom instructions, output language, complete long-transcript processing,
  deterministic references, and text/Markdown/JSON rendering.
- [x] Add model override support through the existing LLM client, keeping its
  current default behavior for other callers. Summary CLI calls disable tools.
- [x] Add CLI flags, validate conflicting modes before retrieval, and update
  the skill/docs for explicit extraction and configurable CLI summaries.
- [x] Run focused Rust/CLI/browser tests, clippy, formatting, and skill validation.
- Delivery: commit and push the validated update to PR #1044.

References remain part of summaries by default. Retrieve the description
separately; missing metadata becomes an explicit warning, never fabricated
references. Model responses must match the structured summary schema and use
timestamps present in the source. Long inputs are summarized in bounded chunks
and reduced before the final summary; never truncate the transcript silently.

## Verification

- 66 focused tests passed: 36 model-client/compression, 19 summarize,
  6 CLI integration, and 5 browser fixtures.
- Clippy passed for both changed crates, including the final hook-isolation
  and summary-budget fixes; formatting and skill validation passed.
- Live extraction of `snErQUyqwCU` returned all 653 timestamp-linked captions.
- The live short JSON summary contained 105 prose words, 3 source-timestamped
  takeaways, and all 5 description reference links with no warnings.
- Summary CLI calls use Claude safe mode after a controlled reproduction
  showed local memory hooks replacing the requested JSON. The existing
  `RealLlm` compression behavior remains unchanged.
- Oversized model drafts are condensed once, then rejected if still over
  budget. The regression test verifies both the rewrite and bounded failure.
