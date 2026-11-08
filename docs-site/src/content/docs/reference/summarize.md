---
title: YouTube transcripts and summaries
description: Retrieve timestamped captions with the summarize module and summarize them with your coding agent.
---

The separate `flare-summarize` module retrieves YouTube captions and produces
summaries with timestamp links and source-backed resource URLs. The CLI uses
agentflare's existing Anthropic API or authenticated Claude CLI configuration.
The builtin `summarize` skill can instead extract captions and let your current
coding agent write the summary, without a second model invocation.

```sh
agentflare summarize 'https://youtu.be/dQw4w9WgXcQ'
agentflare summarize 'https://youtu.be/dQw4w9WgXcQ' --length short --format text
agentflare summarize 'https://youtu.be/dQw4w9WgXcQ' --summary-language French --format json
agentflare summarize 'https://youtu.be/dQw4w9WgXcQ' --prompt 'Focus on implementation steps'
agentflare summarize 'https://youtu.be/dQw4w9WgXcQ' --prompt-file ./focus.txt --model YOUR_MODEL
agentflare summarize 'https://youtu.be/dQw4w9WgXcQ' --extract --json > transcript.json
agentflare summarize 'https://youtu.be/dQw4w9WgXcQ' --extract --format md --timestamps
agentflare summarize 'https://youtu.be/dQw4w9WgXcQ' --language hi --language en
agentflare summarize 'https://youtu.be/dQw4w9WgXcQ' --fetch-only
agentflare skill get summarize
```

Ask the agent to use the summarize skill with a video URL for an overview,
key takeaways, timestamp links, and resources. The skill explicitly uses
`--extract --json`; the standalone CLI summarizes by default.

`--format text|markdown|json` controls output; `md` aliases `markdown` and
`--json` shortcuts JSON. Markdown is the default. `--length` accepts `short`,
`medium`, `long` (default), `xl`, and `xxl`: approximate targets of 100, 250,
500, 1000, and 2000 words, excluding the references section. Short summaries
contain up to three takeaways, medium up to five, and longer summaries up to
eight. An overshooting response is condensed once; if it still exceeds the
word/takeaway budget, the command reports an error instead of silently trimming
the answer. `--prompt` and
`--prompt-file` add focus/style instructions while preserving the structured
summary schema. `--summary-language` controls prose independently of captions.

`--model` overrides `FLARE_SUMMARIZE_MODEL`. API calls otherwise inherit the
existing `FLARE_OUTPUT_MODEL` / legacy `CAVEMAN_MODEL` selection and default.
Without `ANTHROPIC_API_KEY`, an authenticated `claude --print` session is used;
an omitted model keeps the CLI's configured default. Summary sessions use Claude
safe mode to disable local hooks/customizations that can replace model output;
tools, MCP servers, and session persistence are also disabled. No new provider
SDK is required. Use a Claude CLI version that supports `--safe-mode`.

Long transcripts are processed in bounded chunks, then reduced into a final
summary. Empty/malformed model responses and timestamps absent from the source
fail explicitly. Inputs above the 64-chunk limit require separate processing
with `--extract`; the command never silently summarizes just a prefix.

Direct extraction uses the MIT-licensed Rust crate
[`youtube-transcript-api`](https://github.com/bluefielddigital/youtube-transcript-api-rs).
It supplies caption discovery, language selection, parsing, and HTTP errors.
We also evaluated `yt-transcript-rs` and `ytsrt`; the selected crate provides
configurable timeouts and a richer caption/language API without requiring
our own YouTube protocol implementation.

If direct retrieval fails, agentflare opens a dedicated browser session and
tries caption retrieval within the rendered page, then the transcript panel.
It reads the transcript in bounded chunks and closes that session afterward.
The first fallback may install agentflare's existing browser sidecar. Use
`--no-auto-install` to prevent installation, or `--fetch-only` to prevent all
browser use. `AGENTFLARE_BROWSER_NO_AUTO_INSTALL` is also respected.

Extraction JSON (`--extract --json`) includes `video_id`, `url`, `language`,
`source` (`fetch` or `browser`), `generated`, and `segments` containing `start`
(seconds) and `text`. It requires no model and skips description requests.
JSON always preserves timestamps; `--timestamps` adds them to extracted text
or Markdown. Summary-only options conflict with `--extract`.

Summary JSON includes those same fields plus `summary` (an `overview` string
and a `takeaways` array of `text`/`start` objects), `references` (exact URL, label,
source, promotional marker), and `warnings`. Reference discovery reads the
video description and literal URLs in captions. Promotional/affiliate wording
is labeled from source context; links are listed without downloading their
contents. Missing descriptions produce a warning while allowing a caption
summary. Pinned-comment references can be checked by the agent skill when the
creator points there; the standalone CLI does not retrieve comments.

English is the default caption language; repeated `--language` flags specify
ordered alternatives. Regional variants can match the requested base language.

Missing captions, consent/login requirements, and YouTube blocking can still
prevent retrieval. Errors exit nonzero and identify both failed paths when
applicable. The module never substitutes the video description for captions,
and does not download audio or run speech recognition. Summaries describe the
captions, so visual-only information is outside their coverage.

The skill's extraction/summary separation and verification principles are
adapted from [steipete/summarize](https://github.com/steipete/summarize).
The implementation uses the Rust caption client and agentflare browser rather
than installing the Summarize Node.js CLI.

Development checks:

```sh
mbx test -p flare-summarize
node crates/flare-summarize/tests/browser.test.mjs
mbx test -p agentflare-flare-output
mbx test -p agentflare-skill-registry
mbx test --test summarize_cli
```
