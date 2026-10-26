# agentflare — marketing content

Prepared 2026-10-26 against `master` (`Cargo.toml` v1.8.1, CHANGELOG 1.8.1 released 2026-10-06).
Scope: audit, evidence base, positioning, and final site copy for agentflare.dev. No product code was changed.

Contents:

1. Material issues found (fix before or alongside publishing)
2. Evidence base (what we may and may not say)
3. Positioning
4. Site structure and navigation
5. Final copy, section by section
6. Install, first use, compatibility
7. FAQ
8. CTAs, microcopy, footer
9. Appendix: stale items in the current site

---

## 1. Material issues found

Ordered by how badly each undermines the product story. File references are repo-relative.

| # | Severity | Issue | Evidence | Recommended fix |
|---|---|---|---|---|
| 1 | High | **"65% fewer output tokens" is attached to a command that does something else.** `agentflare optimize output` is documented in the CLI as "Compress a markdown file using LLM-based prose compression". It rewrites a file you point it at (memory files, rules, docs). That shrinks *input* tokens the agent reads. It does not make the agent talk less. The site says "Conversation verbosity compression, ~65% fewer tokens" and the stat band says "65% fewer output tokens (`optimize output`, avg)". | `src/cli/optimize.rs:10`; `src/optimize/output.rs:1`; `STATUS.md` ("an opt-in file compression command"); `site/public/index.html` (Optimize card, stat band) | Describe it as file compression. Do not quote an output-token figure for it. See claim C3. |
| 2 | High | **Benchmark sources cited in the README do not exist in the repo.** README says the `optimize output` figures are "Committed in `benchmarks/`/`evals/`" and cites `docs/HONEST-NUMBERS.md`. Neither directory nor file exists. The site repeats "Committed evals". | `ls benchmarks evals` fails; `docs/` holds only `INCIDENT-…`, `audits`, `research`, `superpowers` | Either commit the evals or re-attribute the numbers to the upstream projects (Caveman, Ponytail, lean-ctx) by name and link. Until then, none of the three benchmark rows is "reproducible from this repo". |
| 3 | High | **Per-agent support is overstated.** The site says every listed host "reads hook config directly into its own settings" and `init` prints "hooks + MCP wired". In code, `init` wires hooks only for `claude-code`, `cursor`, `codex`, `opencode`. Windsurf, VS Code Copilot, Cline and Continue receive rules and MCP registration only (Cline also gets a plugin file). | `src/init.rs:120-136` (match on agent); `src/components.rs:964-1070` (MCP wiring for the other four); `src/components.rs:287-364` (rule targets; `continue` has none) | State a support matrix (section 6). Continue gets MCP registration but no rules file. |
| 4 | High | **Codex wiring is described three contradictory ways.** README: `init --agent codex` writes `~/.codex/hooks.json`. Site: Codex "needs its own plugin-loader step". Docs: "the hook wiring itself comes from the plugin manifest, not `init`". Code: `init` calls `wire_codex_hooks()` which writes `~/.codex/hooks.json`. | `src/init.rs:128-130, 644-660`; `docs-site/src/content/docs/getting-started.md`; README "No plugin marketplace required for … Codex" | Needs a maintainer decision and a test on a clean machine. Until then the copy below says only "run `agentflare init --agent codex`" and links to the docs for the plugin step. |
| 5 | Medium | **"No Node, no runtime dependencies" is true of the binary, not of the setup.** `init` installs lean-ctx by piping a remote install script (or `brew`), and the README also lists `npm install -g lean-ctx-bin`. Several features call external services. | `src/tool_install.rs:44-53`; `AGENTS.md` (leanstack) | Say "single binary, no Node required for agentflare itself" and disclose that `init` installs lean-ctx as a separate tool. |
| 6 | Medium | **License contradiction.** Site hero and footer say MIT. `LICENSE` is Apache 2.0 and `Cargo.toml` says `license = "Apache-2.0"`. The README badge says Apache-2.0. | `LICENSE:1-3`; `Cargo.toml:14`; `site/public/index.html` ("Open source · Rust · MIT · Beta", footer "MIT License") | Use Apache-2.0 everywhere. |
| 7 | Medium | **The `agentflare.dev/install.sh` one-liner serves a v1.3.1 installer.** The Worker pins `INSTALL_REF = "v1.3.1"`. Current release is 1.8.1. The hero command uses this endpoint; the install section uses `raw.githubusercontent.com/.../master`. Two different installers are presented as the same thing. The v1.3.1 script's "next step" line also omits opencode. | `site/src/worker.ts:15`; `site/public/index.html` (hero vs install block) | Bump `INSTALL_REF` per release (the comment says to), and use one URL in all copy. The copy below uses `agentflare.dev/install.sh` on the assumption that is fixed. |
| 8 | Medium | **Footer says v1.4.0.** Current is 1.8.1. The Scoop manifest is 1.8.1 but the winget manifest is 1.1.0. | `site/public/index.html` footer; `bucket/agentflare.json:2`; `winget/getappz.agentflare.yaml:2` | Do not hardcode a version in copy. Link to Releases. Update winget or drop it from install copy (this copy omits it). |
| 9 | Medium | **The site omits most of what shipped in 1.5–1.8** and mislabels some of it. Missing: autonomous supervisor (auto-dispatch, PR sweep, auto-merge), agent routing and failover, plan-approval and human gates, durable workflow engine, job sandbox (Linux/WSL2 only), encrypted SQLite, Telegram approvals, browser automation, `pm`. "No daemon required" is true for MCP tools but `agentflare work` and schedules depend on the daemon. | `CHANGELOG.md` 1.8.1; `README.md` "Workflow schedules" (daemon is the only process that executes queued occurrences); `STATUS.md` | New "Coordinate" section below. Mark supervisor and `work` as beta. |
| 10 | Medium | **Safety posture on Windows is weaker than the site implies.** Job sandboxing (bubblewrap) is Linux/WSL2 only. Dispatched jobs on Windows can touch any worktree. | Maintainer note, item #482; CHANGELOG "bubblewrap job sandbox (#420)" | Say "sandboxed jobs on Linux and WSL2". Do not imply Windows parity. |
| 11 | Low | **`KNOWN_ISSUES.md` is stale.** It cites `src/agent_detect.rs` and `src/auth.rs:565` line numbers that no longer match the tree, and says the integration suite "is empty" while `tests/auth_integration.rs` exists. | `KNOWN_ISSUES.md`; `ls src` | Regenerate or delete before linking it publicly. Not linked in the copy below. |
| 12 | Low | **`lean-ctx` repository is referenced as two different owners.** README and `tool_install.rs` use `yvgude/lean-ctx`; `compare.md` links `getappz/lean-ctx`. | `README.md`; `docs-site/src/content/docs/compare.md` | Pick the real upstream. This copy links `yvgude/lean-ctx`, which is what the installer downloads from. |
| 13 | Low | **Version numbers vs. maturity.** `STATUS.md` says Beta and warns that 1.x is not a stability claim. Site and README show "Beta" but the 1.8.1 number invites a stability reading. | `STATUS.md` "Versioning" | Keep the Beta label visible in nav and hero. |

Not found: any telemetry or analytics client in `src/` or `crates/` (searched for telemetry, posthog, sentry, mixpanel, segment). The hits were unrelated words. That supports "no telemetry that we could find", not a guarantee. See P1.

---

## 2. Evidence base

### 2.1 Measurement types (never mix these)

| Type | Meaning | Where we have a number |
|---|---|---|
| Tool-output compression | Fewer tokens in what reads, searches and shell commands return to the agent | lean-ctx (upstream) |
| Input-file compression | Smaller memory/rule/doc files the agent loads | `optimize output` (no number verified) |
| Output-token reduction | The model writes fewer tokens | None verified for agentflare. The 65% figure is from the upstream Caveman project's own prompts. |
| Generated-code reduction | Less code written for the same task | `optimize code` (upstream Ponytail, n=4, Haiku 4.5) |
| Monetary savings | Dollars | Only lean-ctx's self-reported ledger, one user. Not generalizable. |
| Execution time | Wall-clock | Ponytail row ("~27% faster") only |

### 2.2 Claims register

"Public?" means suitable for marketing copy as written in the last column.

| ID | Claim | Source | Conditions and limits | Public? | Wording to use |
|---|---|---|---|---|---|
| C1 | lean-ctx compresses tool output 98.1% (`map`), 96.7% (`signatures`), ~99.99% on cached re-reads | README Metrics table; upstream lean-ctx benchmark (`lean-ctx benchmark report .`) | Third-party tool, installed by agentflare, not built here. Measured on a 50-file repo with the GPT-4o tokenizer. Applies to file reads in those modes, not to every tool call. Not re-run for this audit. | Yes, attributed and qualified | "lean-ctx, the companion tool agentflare installs, reports up to 98% fewer tokens on file reads in its `map` mode (its own benchmark, 50-file repo)." |
| C2 | "34.2M tokens saved, 92% compression, $88.45 saved" | README "Real usage" | One maintainer, lifetime, one project, lean-ctx's own ledger; the dollar figure rests on lean-ctx's pricing assumptions. | Anecdote only. Dollar figure: no | Omit from the hero and stat band. If used, label "one maintainer's project" and drop dollars. |
| C3 | `optimize output`: 65% average output-token reduction (22–87%, 10 prompts) | README; upstream Caveman benchmarks | Command compresses a markdown file; the figure is about model output under a terse-style prompt. Repo has no `benchmarks/` or `evals/`. Doc itself warns of ~1–1.5k input-token overhead per turn that can make it net-negative on terse work. | No | Do not quote. Describe the command only. |
| C4 | `optimize code`: ~54% less code, ~20% cheaper, ~27% faster | README; upstream Ponytail benchmark | 12 tasks, FastAPI + React repo, Haiku 4.5, n=4 runs. Other models, languages and agents untested. README says "100% safe" while the site says "no regressions across the 4 tasks measured"; use the narrower one. | With heavy qualifiers, secondary placement | "In the upstream project's benchmark (12 feature tasks, one FastAPI + React repo, Haiku 4.5), agents wrote about 54% less code. Your results will vary." |
| C5 | Built-in memory persists decisions across sessions (SQLite + FTS5) | `STATUS.md` (stable); `tests/memory_cli.rs`; `src/memory/` | Local store. No effectiveness benchmark. | Yes (capability, not effect size) | "Remembers decisions between sessions." |
| C6 | `agentflare cost` reports spend from session transcripts | `src/cost.rs`; `data/anthropic-pricing.json`; `STATUS.md` | Reads Claude Code JSONL transcripts under `~/.claude/projects/`. Claude Code only. Pricing table is bundled and can go stale. | Yes, scoped to Claude Code | "See what your Claude Code sessions cost." |
| C7 | Work items and claims stop two agents taking the same task | `item`, `claim` MCP tools; `src/claims.rs`; `src/claim_liveness.rs`; `STATUS.md` (actively changing) | Local SQLite per machine; multi-workstation claim markers use GitHub (CHANGELOG #632). Schema may change. | Yes, marked beta | "Claim an item before you work it." |
| C8 | Git shim blocks writes to `master`/`main` from the wrong worktree | `crates/agentflare-shim`, `crates/flare-git-shim`; README "Git-aware PATH shim"; maintainer notes on guard layers | Works when the shim is on PATH and the agent is detected. Known deferred gaps exist. Windows hook coverage differs. | Yes, scoped ("guardrail, not a security boundary") | "A guardrail against accidental commits to your default branch." |
| C9 | Review: citations verified against the diff, findings deduped and tagged CONFIRMED / UNIQUE / DISPUTED / UNVERIFIED | README; `crates/flare-consensus`; `src/review/` | Finding schema "not finalized" (STATUS). No accuracy benchmark. | Yes, capability only | "Checks that each finding actually points at a line in the diff." |
| C10 | `agentflare work`: claim → worktree → headless agent → comment + PR, unattended | README; `src/cli/work.rs`; CHANGELOG | Beta. History of dispatch bugs (process storms, duplicate PRs, worktree wipes) fixed in 1.8.x. Sandbox is Linux/WSL2 only. | Yes, labelled beta, no "hands-off" promise | "Hand a work item to a headless agent and get a PR back. Beta." |
| C11 | Search: one MCP tool, 17 sources | README; `src/mcp_server/search.rs` | Sources call third-party public APIs (GitHub, CoinGecko, Open-Meteo, Frankfurter, World Bank, SEC, Bluesky…). Some need tokens. Not "local". The count of 17 was taken from the README, not recounted. | Yes, with network note | "One search tool across many sources, including your own memory and items." |
| C12 | Encrypted auth vault / SQLCipher option | `src/auth_crypt.rs` (AES-GCM, PBKDF2); CHANGELOG #824 | `KNOWN_ISSUES` records an empty-passphrase failure mode in non-interactive shells (may be stale). | Yes, no "military-grade" style claims | "Credentials are stored encrypted on your machine." |
| C13 | Local-first | `STATUS.md`; SQLite stores under `~/.agentflare`; dashboard binds `127.0.0.1` by default (`src/cli/serve.rs:10`) | `serve --host 0.0.0.0` is possible and requires an explicit flag/auth (`serve.rs:16`). Telegram, GitHub, search, update checks and any LLM-backed command use the network. | Yes, with the network disclosure | See P1 and FAQ. |
| C14 | Prebuilt binaries: Linux x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64 | `.github/workflows/release.yml:17-31`; `bucket/agentflare.json` | Windows binary is not Authenticode-signed (README). `install.sh` supports Linux and macOS only. | Yes | See section 6. |
| C15 | Release binaries carry cosign signatures and SLSA L3 provenance | README "Verifying release binaries"; `release.yml` | Workflow internals not re-audited. | Yes, link to the verify steps | "Signed with cosign; SLSA provenance attached." |
| C16 | Supported agents: Claude Code, Codex, Cursor, Windsurf, VS Code Copilot, Cline, Continue, opencode | `src/init.rs`, `src/components.rs`, `crates/agent-registry/src/registry.rs` | Depth differs by agent (section 6). Registry also lists Gemini CLI and Copilot CLI for launching/routing, not for `init`. | Yes, via the matrix | See section 6. |
| C17 | Single static binary, no Node | README | "Static" not verified in this audit (no `crt-static` setting found). Binary needs no Node. lean-ctx is separate. | Partly | "One binary. No Node required." Drop "static". |

### 2.3 Privacy, network and security: what the code supports

| ID | Statement | Support | Caveat |
|---|---|---|---|
| P1 | We found no telemetry or analytics client in the source. | Source search, see section 1 | A text search is not an audit. Publish only after a maintainer confirms. |
| P2 | State lives on your machine: SQLite under `~/.agentflare` and the platform data dir. | `STATUS.md`; `KNOWN_ISSUES.md` (state paths) | Paths vary by platform. |
| P3 | Network calls happen only when you use a feature that needs one: update check (GitHub releases, cached), search sources, GitHub operations, Telegram/Slack/Discord notifications, Claude usage lookup (`api.anthropic.com/api/oauth/usage`), Cloudflare AI gateway option, `flare-docs` lookups (docs.rs / npm / PyPI), any LLM-backed command. | `src/update/github.rs`; `src/mcp_server/search.rs`; `src/channels.rs`; `src/claude_usage.rs`; `docs-site/.../compare.md` | Not exhaustive. Whether update checks can be disabled was not verified. |
| P4 | The agent you wire still talks to its own model provider as before. agentflare adds no model of its own to that path. | Architecture (hooks + MCP) | `flare-proxy` and the `decide` layer are exceptions when enabled (provider routing; Jev via OpenRouter). Both are opt-in/beta. |
| P5 | The dashboard listens on 127.0.0.1 unless you change it. | `src/cli/serve.rs:9-16` | "Read-only" was taken from the README and not re-verified for every route. |
| P6 | Installer verifies SHA-256 and refuses to install unverified. | `install.sh` `verify_checksum`, fail-closed branch | Checksum comes from the same release origin; cosign/SLSA covers substitution. |
| P7 | `init` changes your agent's config files (hooks, MCP, rules) and can install lean-ctx. | `src/init.rs`; `src/components.rs`; `src/tool_install.rs` | Steps such as rule refresh and gateway registration prompt unless `-y`. |
| P8 | `uninstall` removes what `init` wrote. | `src/uninstall.rs`; README | `install.sh --uninstall` removes only the binary, by its own message. Run `agentflare uninstall` first. Test coverage for uninstall is unclear (`KNOWN_ISSUES`, may be stale). |

### 2.4 Things we must not claim

- Any customer, testimonial, user count, star count or download number. None exist in the repo.
- A combined savings percentage, or dollars saved for a typical user.
- That agentflare makes Codex, Cursor, Windsurf, Cline or Continue cheaper by a measured amount. All benchmarks used Claude Code.
- "Hands-off", "autonomous", "production-ready", "enterprise" or "secure by default".
- Roadmap items. The repo has epics and specs, none of which is a public commitment.

---

## 3. Positioning

### 3.1 Audience and job

**Primary audience:** developers who use a terminal or IDE coding agent every day (Claude Code first, then Codex, Cursor, opencode) and have started to feel two kinds of friction: sessions that burn context and money re-reading things, and a second agent (or a second session) that steps on the first.

**Most important job:** "Let me keep working with my agent for longer without paying for the same context twice, and without losing what we decided."

**Secondary job (adopt later):** "Let me run more than one agent on a repo without them colliding."

Not the audience for now: teams wanting a hosted, multi-seat PM tool with a browser UI. The comparison page already says so honestly.

### 3.2 Category and value proposition

**Category:** a local toolkit that sits beside your coding agent. Short form: *agent toolkit* (not "platform", not "orchestrator").

**One-sentence value proposition:** agentflare is a single command-line program that makes coding agents cheaper to run, remembers what you decided between sessions, and, when you are ready, keeps several agents from colliding on the same repo.

### 3.3 Differentiators (each with evidence)

1. **Works with the agent you already use.** One `init` command for eight agents; no new editor or agent to adopt. (C16)
2. **Two layers you adopt in order.** Optimization works on day one for one agent. Coordination is opt-in. (STATUS, README)
3. **Local-first state.** Memory, items, claims and the dashboard live on your machine; the dashboard binds to localhost by default. (C13, P2, P5)
4. **Honest about its numbers.** Every figure is attributed, qualified and scoped to what it measured. Few tools in this space do that, and it is a real trust differentiator. (README Metrics approach; STATUS)
5. **Guardrails you can see.** Branch guard, plan-approval gate, fail-closed command approval, pre-handoff secret scan, signed releases. (C8, C15, CHANGELOG 1.8.1)

Do not lead with the count of features. The README lists about twenty capabilities; the page should teach three ideas.

### 3.4 Explaining two layers without overwhelming

Use one frame, repeated: **"Make one agent cheaper. Then make several agents safe."**

- Stage 1, *Optimize*: the page's first full section. Five capabilities only (tool-output compression, memory, cost view, code-minimalism rules, prose compression for your own files). Everything else is "later".
- Stage 2, *Coordinate*: introduced with a concrete situation ("two agents, one repo"), then claims, review and handoffs. A second row, "Automation (beta)", holds `work`, supervisor, schedules.
- Everything else (search, vault, vent, proxy, apps, browser) lives behind a docs link, not on the homepage.

Unfamiliar terms are defined where first used: MCP ("the standard way agents call tools"), hook ("a command your agent runs at set moments, such as session start"), worktree ("a second checkout of the same repo, so two agents do not edit the same files"), claim.

### 3.5 Primary conversion

**Install and wire one agent** (`curl … | sh`, then `agentflare init --agent <yours>`). Secondary: read the docs; view the source on GitHub. No signup, no waitlist, no email capture exists, so none is invented.

### 3.6 Voice

"impeccable.style" was not defined anywhere in the repo or this session, so this is interpreted as a bar of precision and polish. If it refers to a specific style guide, send it and the copy will be re-checked against it.

The bar: plain developer English, concrete nouns, short sentences, no superlatives, no emoji, no exclamation marks, every number carries its unit and its source. Where a claim needs a qualifier, the qualifier sits in the same sentence.

---

## 4. Site structure and navigation

**Top navigation (left to right):** `agentflare` (logo) · Optimize · Coordinate · Numbers · Install · Docs · GitHub · **Install** (button)

Keep the current six anchors. Add a "Beta" badge beside the logo linking to `STATUS.md`.

**Homepage sections, in order:**

1. Hero
2. Compatibility strip ("Works with")
3. The problem in two sentences
4. How it fits together (two layers)
5. Optimize
6. Coordinate
7. A realistic first hour (workflow walkthrough)
8. Numbers
9. Local-first, and what leaves your machine
10. Install and first use
11. FAQ
12. Closing CTA
13. Footer

**Supporting pages (existing docs site):** Getting started · Concepts · CLI · MCP tools · Guides · Compare · Status (new, mirrors `STATUS.md`) · Security and verification (new, mirrors README verification section).

The stat band from the current page is removed. Its four numbers mix four different measurements, which the Numbers section now separates.

---

## 5. Final copy

### 5.1 Hero options

**A — Recommended**
- Headline: **Make your coding agent cheaper to run. Make several of them safe to run together.**
- Subheadline: agentflare is one command-line program that trims what your agent reads, remembers what you decided between sessions, and keeps multiple agents from editing the same work. It works with the agent you already use.

**B**
- Headline: **Keep your coding agent focused, and keep your agents out of each other's way.**
- Subheadline: Compress tool output, remember decisions across sessions, and share a backlog and branch guard between agents. Local, open source, beta.

**C**
- Headline: **The toolkit that sits beside Claude Code, Codex and Cursor.**
- Subheadline: Fewer wasted tokens in one session. Fewer collisions across many. One binary, wired in with one command.

**Recommendation: A.** It names both jobs in the order a newcomer needs them (one agent first, several later), avoids numbers in the headline, and contains no claim that needs a footnote. B is softer and less specific. C leads with brand names, which reads as a compatibility list rather than a benefit.

### 5.2 Hero block (final)

> **Eyebrow:** Open source · Apache-2.0 · Beta
>
> **H1:** Make your coding agent cheaper to run. Make several of them safe to run together.
>
> **Sub:** agentflare is one command-line program that trims what your agent reads, remembers what you decided between sessions, and keeps multiple agents from editing the same work. It works with the agent you already use.
>
> **Command:** `curl -fsSL https://agentflare.dev/install.sh | sh`
> **Then:** `agentflare init --agent claude-code`
>
> **Primary button:** Install agentflare   **Secondary button:** Read the docs
>
> **Microcopy under buttons:** Linux, macOS and Windows. Free and open source. It edits your agent's config files, and `agentflare uninstall` reverses that. Currently in beta: see what's stable.

### 5.3 Compatibility strip

> **Works with** Claude Code · Codex · Cursor · opencode · Windsurf · VS Code (Copilot) · Cline · Continue
>
> *Depth of integration varies by agent. See the compatibility table.*

### 5.4 The problem

> **Heading:** Two things go wrong when you use coding agents all day.
>
> **One.** Every turn, your agent re-reads files, command output and search results. You pay for those tokens again and again, and long sessions run out of room. When a session ends, the decisions you made in it are gone.
>
> **Two.** Once you run a second agent, or a second session, they can pick up the same task, edit the same files, or commit to your default branch from the wrong checkout. Nothing tells either one what the other is doing.
>
> agentflare addresses the first problem on its own, and the second when you are ready for it.

### 5.5 How it fits together

> **Heading:** Two layers. Adopt them in order.
>
> **Layer 1: Optimize (works with one agent).** Compress what the agent reads, remember what you decided, see what it costs. Install it and forget it.
>
> **Layer 2: Coordinate (when you run more than one).** A shared backlog, claims so two agents do not take the same task, review of each other's work, and handoffs with context. Opt in tool by tool.
>
> **Footnote:** Both layers are available to any agent that supports MCP, the standard way agents call external tools. Hooks, where an agent supports them, add automatic behavior such as session-start context.

### 5.6 Optimize

> **Eyebrow:** Layer 1
> **Heading:** Spend fewer tokens on things the agent has already seen.
> **Intro:** These pieces work for a single agent. `agentflare init` sets up the first two for you.

**Card 1 — Compressed tool output (lean-ctx)**
Reads, searches and shell output are cached and reduced before they reach your model. agentflare installs lean-ctx, an open-source companion tool, and wires it into your agent. On file reads in its most aggressive mode, lean-ctx reports reductions of up to 98% in its own benchmark. See Numbers for conditions.

**Card 2 — Memory across sessions**
Decisions, facts and preferences are saved to a local database and recalled in the next session, so you stop re-explaining the project. Search it with `agentflare memory search "<query>"`.

**Card 3 — Cost view**
`agentflare cost` totals what your Claude Code sessions have cost, by project and day, from the transcripts already on your disk. Claude Code only.

**Card 4 — Code-minimalism rules (`optimize code`)**
Adds a short set of instructions, delivered by hooks in Claude Code and Codex, that nudge the agent toward the smallest change that works. Based on the Ponytail project. The upstream benchmark measured less generated code; it did not measure quality in your codebase, so review the diffs as you always do.

**Card 5 — Prose compression for your own files (`optimize output`)**
`agentflare optimize output <file.md>` rewrites a markdown file (for example a long rules or notes file) in a terser style so the agent reads fewer tokens each time it loads it. It uses a language model to do the rewrite and can keep a backup. `optimize retrieve` brings back the original text.

> **Honesty line under the cards:** Each piece saves a different thing, so we do not add the savings together.

### 5.7 Coordinate

> **Eyebrow:** Layer 2 · Beta
> **Heading:** When two agents work on one repo, give them a shared picture.
> **Intro:** A local backend, exposed over MCP, that any MCP-capable agent can use. No account. Your state stays on your machine unless you connect GitHub or a chat channel.

**Situation, in plain terms:** You ask one agent to fix the login bug and another to add rate limiting. Both open `auth.rs`.

**What agentflare adds:**

**Work items and claims.** A shared backlog on your machine. An agent claims an item before starting, and a second agent sees it is taken.

**Worktrees.** Each claimed item gets its own checkout of the repo, so parallel agents do not overwrite each other's files.

**Branch guard.** A thin wrapper around `git` that refuses writes to your default branch from the wrong checkout. It is a guardrail against mistakes, not a security boundary.

**Review.** Agents submit findings against a diff. agentflare checks that each finding points at a real line in the diff, merges duplicates, and labels each CONFIRMED, UNIQUE, DISPUTED or UNVERIFIED.

**Artifacts and handoffs.** Specs and plans are saved as versioned pages that survive the session. A handoff passes an item and its context from one agent to another, with a thread you can read.

**Automation (beta)**
`agentflare work` claims an item, creates a worktree, runs a headless agent and opens a pull request with a comment on the item. A background daemon can schedule workflows and watch open PRs. Human gates are available: plan approval before dispatch, command approval, and approval over Telegram.
*Automation is the newest part of agentflare. Its flags and on-disk formats can change between releases. Run it on branches you can discard, and read the status page first.*

**Also in the box:** a read-only local dashboard (`agentflare serve`), a multi-source search tool, an encrypted credential vault, and friction logging (`vent`). Details in the docs.

### 5.8 A realistic first hour

> **Heading:** What the first hour looks like.
> **Note:** Commands below exist in the current CLI. Output is described, not pasted, because it varies by project.

1. **Install and wire your agent.**
   `curl -fsSL https://agentflare.dev/install.sh | sh` then `agentflare init --agent claude-code`. `init` writes rule files, installs lean-ctx if it is missing, and registers hooks and the MCP server in your agent's config. It skips anything already in place, so running it again is safe.
2. **Work as usual.** Restart the agent so it picks up the config. File reads and shell output now pass through lean-ctx.
3. **Ask the agent to remember something.** For example: "We use SQLite, not Postgres, for local state." Next session, `agentflare memory context` shows it.
4. **Check the bill.** `agentflare cost` shows spend per project from your Claude Code transcripts.
5. **Later, add a second agent.** Create two work items, let each agent claim one, and look at them in `agentflare serve`. Open the printed local address in a browser.
6. **Undo anything.** `agentflare uninstall` removes the hooks, rules and MCP entries `init` added.

### 5.9 Numbers

> **Eyebrow:** Numbers
> **Heading:** Every figure here is measured by someone, on something specific.
> **Intro:** We list who ran each benchmark, what it measured, and where it stops applying. We do not combine them, and none of them is a prediction for your project.

| What it measures | Result | Who measured it and how | Limits |
|---|---|---|---|
| **Tool-output compression** (lean-ctx) | 98.1% fewer tokens on file reads in `map` mode; 96.7% in `signatures` mode; near-total reduction on repeated reads of an unchanged file | lean-ctx's own benchmark (`lean-ctx benchmark report .`), a 50-file repository, GPT-4o tokenizer | Applies to file reads in those modes. Other tool calls compress less. We have not independently re-run it. |
| **Generated code volume** (`optimize code`) | About 54% less code on average across 12 feature tasks; about 20% lower cost and 27% faster in the same runs | The Ponytail project's benchmark: FastAPI + React repository, Claude Haiku 4.5, 4 runs | One repository, one model, small sample. It measures how much code the agent wrote, not whether the code was better. |

> **Not included on purpose:** an output-token figure for `optimize output`. The command compresses files you give it, and we have not published a measurement of that effect.
>
> **Check your own:** `lean-ctx gain` · `agentflare cost` · `agentflare memory context`

*(Optional, only if a maintainer approves: "One maintainer's project has saved 34.2M tokens through lean-ctx over its lifetime. That is one data point, not a benchmark.")*

### 5.10 Local-first, and what leaves your machine

> **Heading:** Your data stays local. Some features reach out, and you choose which.
>
> **Stays on your machine.** Memory, work items, claims, review findings, artifacts, and the credential vault are stored in local SQLite files. The dashboard listens on `127.0.0.1` unless you tell it otherwise.
>
> **Reaches the network only when you use it.** Update checks (GitHub releases), GitHub operations, the search tool and its public data sources, documentation lookups from docs.rs, npm and PyPI, Telegram, Slack and Discord notifications, and any command that uses a language model. Your agent continues to talk to its own model provider as before.
>
> **Installs software.** `agentflare init` installs lean-ctx, a separate open-source tool, unless it is already present.
>
> **Verify what you download.** The installer checks SHA-256 and refuses to continue if the checksum is missing. Release binaries are signed with cosign and carry SLSA provenance; the steps are in the docs.
>
> *We have not found analytics or telemetry code in the source.* (Publish this sentence only after a maintainer confirms.)

### 5.11 Closing CTA

> **Heading:** Start with one agent.
> **Body:** Install, run `init`, and keep working as usual. Add coordination when a second agent shows up.
> **Buttons:** Install agentflare · Read getting started · View on GitHub
> **Microcopy:** Beta software. Check what's stable before you build automation on it.

---

## 6. Install, first use, compatibility

### 6.1 Prerequisites

| Need | Detail |
|---|---|
| An agent | Any in the table below. agentflare does not include one. |
| OS | Linux (x86_64, aarch64), macOS (x86_64, Apple Silicon), Windows (x86_64) |
| For the install script | `curl`, `tar`, and `sha256sum` or `shasum` (Linux/macOS) |
| For lean-ctx (installed by `init`) | `curl`, or Homebrew |
| For building from source | Rust (toolchain pinned in `rust-toolchain.toml`) |
| For `agentflare work` isolation | Linux or WSL2 (job sandbox). Windows runs jobs without it. |

### 6.2 Install

**Linux and macOS**
```bash
curl -fsSL https://agentflare.dev/install.sh | sh
```
Downloads the release for your platform and verifies its SHA-256 checksum. If the checksum cannot be fetched or does not match, the script stops. Installs to `~/.local/bin`; set `AGENTFLARE_INSTALL_DIR` to change that.

**Homebrew (macOS, Linux)**
```bash
brew tap getappz/agentflare
brew install agentflare
```

**Windows, Scoop**
```powershell
scoop bucket add agentflare https://github.com/getappz/agentflare
scoop install agentflare
```
The Windows binary is not Authenticode-signed, so SmartScreen or antivirus may warn. You can verify it with cosign instead.

**Windows, from source**
```powershell
git clone https://github.com/getappz/agentflare
cd agentflare
.\install.ps1
```

**Any platform with Rust**
```bash
cargo install --git https://github.com/getappz/agentflare
```

Check it worked: `agentflare --version`

### 6.3 First use

```bash
agentflare init --agent claude-code     # or codex, cursor, opencode, windsurf, vscode-copilot, cline, continue
```
Restart the agent afterwards. Pass `-y` to skip confirmation prompts. Re-running is safe.

Then try:
```bash
agentflare doctor            # check the install
agentflare cost              # Claude Code spend, from local transcripts
agentflare memory context    # what memory would recall now
agentflare serve             # local read-only dashboard
```

Remove everything:
```bash
agentflare uninstall         # removes what init wrote
# then remove the binary, e.g. curl -fsSL …/install.sh | sh -s -- --uninstall
```

### 6.4 Compatibility matrix

Derived from `src/init.rs` and `src/components.rs`. "Hooks" are commands the agent runs at set moments (session start, before a tool call). "MCP" registers agentflare's tools with the agent. Cells marked † were read from code but not run on a clean machine.

| Agent | `init --agent` | Rules file | Hooks | MCP tools | Notes |
|---|---|---|---|---|---|
| Claude Code | `claude-code` | Yes | Yes | Yes | Most tested. Only agent with `agentflare cost` support and all published benchmarks. |
| Codex | `codex` | Yes | Yes† (`~/.codex/hooks.json`) | Yes | Documentation currently also mentions a Codex plugin step. Follow the docs page until clarified. |
| Cursor | `cursor` | Yes | Yes† (`.cursor/hooks.json`) | Yes | |
| opencode | `opencode` | Yes | Yes† (plugin) | Yes | Rules wired through `opencode.jsonc`. |
| Windsurf | `windsurf` | Yes | No | Yes | |
| VS Code (Copilot) | `vscode-copilot` | Yes (`.github/copilot-instructions.md`) | No | Yes (`.vscode/mcp.json`) | Project-level. |
| Cline | `cline` | Yes (`.clinerules`) | Plugin† | Yes | |
| Continue | `continue` | No | No | Yes (`.continue/mcpServers`) | MCP only. |

*Also recognized for launching and routing in `agentflare agents` (not wired by `init`): Gemini CLI and Copilot CLI.*

---

## 7. FAQ

**Do I need to run more than one agent to benefit?**
No. Compressed tool output, memory and the cost view help a single agent. Coordination is for when you run more than one.

**Will it change my agent's files?**
Yes. `init` adds rule files, hooks and an MCP entry to your agent's configuration, and installs lean-ctx if missing. It skips what is already there. `agentflare uninstall` removes what it added.

**Is my code or prompt sent anywhere new?**
agentflare does not add a model to your agent's path, so your prompts still go to the provider your agent already uses. Features that call outside services (GitHub, search sources, chat notifications, update checks, LLM-backed commands) do so only when used. See "Local-first" above for the list.

**Does it cost anything?**
agentflare is free and open source under Apache-2.0. Features that call a language model use your own provider account.

**How much will I save?**
We cannot say. The published figures come from specific benchmarks (see Numbers). Run `lean-ctx gain` and `agentflare cost` on your own work.

**Is it stable?**
It is beta. The optimization layer and memory are the most settled and are listed as stable in STATUS. Work items, review, artifacts, handoffs and the MCP tool surface can change between releases, so pin a version before scripting against them.

**Does it work on Windows?**
Yes, with a prebuilt x86_64 binary via Scoop or a source build. Some protections, notably job sandboxing for `agentflare work`, are Linux/WSL2 only.

**What happens if the daemon is not running?**
MCP tools and the CLI work without it. The daemon is needed for scheduled workflows, the supervisor, and `agentflare work` dispatch.

**Can the automation merge things without me?**
The supervisor can poll checks and, if configured, merge. Plan-approval and command-approval gates exist so you can require a human decision first. Treat automated dispatch as beta and keep approval gates on until you trust the setup.

**What if it breaks my agent's config?**
Back up the file first if you are cautious. `init` preserves existing settings and entries, and `uninstall` removes only agentflare's. Report a problem on GitHub.

**How is this different from running lean-ctx by itself?**
lean-ctx covers tool-output compression for one agent and is a smaller install. agentflare installs and wires it, then adds memory, cost reporting and the coordination layer. If you only want compression, use lean-ctx alone.

**How is this different from Linear or Jira?**
Those are hosted, multi-person tools. agentflare's tracker is local to your machine, needs no account, and is built for agents to read and write. It does not replace a team's shared planning tool.

**Who builds it?**
The getappz organization on GitHub. Source, issues and releases are public.

---

## 8. CTA labels, microcopy, footer

### 8.1 CTA labels

| Where | Label | Microcopy |
|---|---|---|
| Nav button | Install | |
| Hero primary | Install agentflare | Linux, macOS, Windows. Free, open source, Apache-2.0. |
| Hero secondary | Read the docs | 5-minute getting started |
| Copy button | Copy | (toast) Copied |
| Optimize section | See how to set it up | |
| Coordinate section | Read the coordination guide | Beta: flags and formats may change. |
| Numbers section | How we measured | Links to benchmark sources |
| Install section | Copy install command | Checksum-verified. Stops if verification fails. |
| FAQ | Ask a question on GitHub | |
| Closing | Install agentflare · View on GitHub | |

### 8.2 Page metadata

- **Title:** agentflare: a toolkit for coding agents
- **Meta description:** An open-source command-line toolkit that compresses what your coding agent reads, remembers decisions across sessions, and coordinates several agents on one repo. Works with Claude Code, Codex, Cursor, opencode and more.
- **Open Graph title:** agentflare: make your coding agent cheaper to run
- **Open Graph description:** One binary, wired in with one command. Beta.

### 8.3 Footer

> **agentflare** — A toolkit for coding agents. One binary, no Node required.
>
> **Product:** Optimize · Coordinate · Numbers · Install
> **Resources:** Docs · CLI reference · MCP tools · Compare · Status (what's stable) · Changelog
> **Trust:** Security policy · Verify a release · Apache-2.0 license
> **Community:** GitHub · Issues · Contributing
>
> © 2026 agentflare. Apache-2.0. Beta software: see Status before depending on any specific flag or tool name.
>
> *Do not print a version number in the footer. Link "Changelog" instead.*

---

## 9. Appendix: current site content to change

| Current text on `site/public/index.html` | Problem | Replace with |
|---|---|---|
| "Open source · Rust · MIT · Beta" and "MIT License" | License is Apache-2.0 | Apache-2.0 |
| "v1.4.0 · beta · local-first · daemon optional" | Stale; current is 1.8.1 | Remove version; keep "Beta" |
| Stat band: 98.1% / 65% / 54% / 34.2M | Four different measurements in one row; 65% mislabelled; 34.2M is one user | Remove; use the Numbers table |
| "optimize output — Conversation verbosity compression, ~65% fewer tokens" | Command compresses markdown files | Card 5 in 5.6 |
| "Companion tool… up to 99%" | Rounded up from 98.1% / 99.99% cached | "up to 98% (map mode)" with source |
| "Committed evals" | No such directory in repo | "The upstream project's benchmark" |
| "Confidence: High" column | Self-assigned rating, unverified here | Remove column; show "Limits" instead |
| "Every host above reads hook config directly into its own settings" | Four of eight agents have no hook wiring | Compatibility matrix (6.4) |
| "✓ rules written · lean-ctx installed · hooks + MCP wired · memory ready" | Claims hooks for hosts that get none | Describe, do not simulate; or show real output per agent |
| "No daemon required" (Coordinate intro) | True for MCP tools; false for `work`, schedules, supervisor | "Core tools need no daemon; automation does" |
| "vault & proxy… flare-proxy" | README says proxy is not wired into `init` | Move to docs |
| Hero command on `agentflare.dev/install.sh` vs install section on raw GitHub | Different installers (Worker pinned to v1.3.1) | One URL; bump `INSTALL_REF` |
| "Works with VS Code" | The integration targets GitHub Copilot in VS Code | "VS Code (Copilot)" |
| "Run coding agents lean. Coordinate a whole crew of them." | "Whole crew" and "lean" overreach; automation is beta | Hero A in 5.1 |
