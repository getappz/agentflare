---
title: YouTube transcripts and summaries
description: Retrieve timestamped captions with the summarize module and summarize them with your coding agent.
---

The separate `flare-summarize` module retrieves YouTube captions. The builtin
`summarize` skill tells your existing coding agent how to summarize them; no
second LLM provider or summarization API key is required.

```sh
agentflare summarize 'https://youtu.be/dQw4w9WgXcQ'
agentflare summarize 'https://youtu.be/dQw4w9WgXcQ' --json > transcript.json
agentflare summarize 'https://youtu.be/dQw4w9WgXcQ' --language hi --language en
agentflare summarize 'https://youtu.be/dQw4w9WgXcQ' --fetch-only
agentflare skill get summarize
```

Ask the agent to use the summarize skill with a video URL for an overview,
key takeaways, and timestamp links. The CLI itself emits the transcript,
not an LLM-generated summary.

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

JSON includes `video_id`, `url`, `language`, `source` (`fetch` or `browser`),
`generated`, and `segments` containing `start` (seconds) and `text`.
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
mbx test -p agentflare-skill-registry
mbx test --test summarize_cli
```
