# agentflare — marketing content

Rebuilt 2026-10-26 against `master` at `765e13cf` (includes PR #970, which added `ARCHITECTURE.md` and corrected the README). `Cargo.toml` v1.8.1, CHANGELOG 1.8.1 released 2026-10-06.
Scope: audit, evidence base, positioning, and final site copy for agentflare.dev. No product code was changed.

Follow-up verification for item #780: upstream [lean-ctx README](https://github.com/yvgude/lean-ctx#benchmarks) and [BENCHMARKS.md](https://github.com/yvgude/lean-ctx/blob/main/BENCHMARKS.md) withdraw the historical read-mode figures from product copy. The final copy below supersedes the earlier compression-number recommendation. Prerequisite decisions and runtime hook counts are recorded in section 10.

Sources of truth, in order of trust: the code; `ARCHITECTURE.md` (source-cited, 2026-10-26); `STATUS.md`; the README. The existing site and docs pages are treated as claims to verify.

Contents:

1. Material issues found
2. Evidence base
3. Positioning
4. Site structure and navigation
5. Final copy
6. Install, first use, compatibility
7. FAQ
8. CTAs, microcopy, footer
9. Appendix: current site content to change
10. Open items before publishing

---

## 1. Material issues found

Ordered by how badly each undermines the product story. Paths are repo-relative. "Fixed by #970" means the README or a root doc was corrected; the website and docs-site were not touched by that PR.

| # | Severity | Issue | Evidence | Recommended fix |
|---|---|---|---|---|
| 1 | High | **The dashboard is not read-only, and the site calls it read-only.** `agentflare serve` starts an execution host: `POST /api/jobs`, messages, chat, item/job controls and a gate reset are all routed, and starting it can run queued work and scheduled workflows. There is no general authentication. Non-loopback binding needs `--yes-expose`, which is an acknowledgement, not access control. `agentflare mcp --http` also has no authentication. The CLI help string still says "Serve the read-only agentflare dashboard", and `site/public/index.html` says "read-only dashboard". The README was fixed by #970. The earlier draft of this document repeated the error. | `src/dashboard/server.rs:692` (`.post(submit_job_handler)`); `src/cli/serve.rs:3,15-16`; `ARCHITECTURE.md` "Resolved contradiction — the dashboard is not read-only"; `site/public/index.html` | Fix the CLI help string and the site. Copy below calls it a "local dashboard and execution host" and tells people to keep the default `127.0.0.1` binding. |
| 2 | High | **"65% fewer output tokens" is attached to a command that does something else.** `agentflare optimize output compress` rewrites a markdown file you point it at using a language model. That shrinks input the agent reads. It does not make the agent talk less. The site says "Conversation verbosity compression, ~65% fewer tokens" and the stat band says "65% fewer output tokens". | `src/cli/optimize.rs:9-10`; `ARCHITECTURE.md` capability table ("Compress a Markdown work product…"); `STATUS.md` ("an opt-in file compression command") | Describe it as file compression. Do not quote an output-token figure for it. See C3. |
| 3 | High | **Benchmark sources cited in the README do not exist in the repo, and never did.** README says the `optimize output` figures are "Committed in `benchmarks/`/`evals/`" and cites `docs/HONEST-NUMBERS.md`. No `bench*` or `eval*` directory exists, and git history shows no deleted `benchmarks/`, `evals/` or `HONEST-NUMBERS.md`. `ARCHITECTURE.md` independently classes all README performance numbers as "attributed claims, not measurements". | `find . -iname 'bench*' -o -iname 'eval*'` (empty); `git log --all --diff-filter=D` on those paths (empty); `README.md:45,176,186`; `ARCHITECTURE.md` "Confidence and verification boundaries" | Re-attribute the figures to the upstream projects (lean-ctx, Caveman, Ponytail) and link their reports. Do not say "committed". Remove the "Confidence: High" column. |
| 4 | High | **Per-agent support is overstated.** The site says every listed host "reads hook config directly into its own settings" and that `init` ends with "hooks + MCP wired". `init` wires hooks only for `claude-code`, `cursor`, `codex` and `opencode`. Windsurf, VS Code Copilot, Cline and Continue get rules and MCP only (Cline also gets a plugin file; Continue gets no rules file). | `src/init.rs:120-136`; `src/components.rs:287-364,964-1070`; `ARCHITECTURE.md` ("Host support is not uniform") | Show the compatibility matrix (section 6.4) instead of a blanket claim. |
| 5 | High | **Codex setup is documented two contradictory ways; the code and the README now agree, the docs pages do not.** Since commit `ea7dfa99` (2026-09-26) `init --agent codex` writes six lifecycle hooks and three `optimize code` hooks to `~/.codex/hooks.json`, and registers the MCP server with `codex mcp add flare`. README (after #970) and `ARCHITECTURE.md` say this. `docs-site/.../cli.md:28-31` and `getting-started.md:40-44` still say the plugin must be installed first and that hook wiring "comes from the plugin manifest, not `init`". The plugin ships the same hook set, so installing both may fire each hook twice. The README also now notes that Codex's native Bash hook omits the exit status, so it cannot prove a test passed. | `src/init.rs:128-130,644-690`; `.codex-plugin/plugin.json`; `hooks/hooks.json`; `README.md` (Codex section); docs pages above | Fix the two docs pages. Test a clean Codex install with and without the plugin and count hook runs. Copy below says `init` alone. |
| 6 | Medium | **"Single static binary, no Node" is wrong as a site claim.** README (after #970) says "the core CLI needs no Node; integrations and optional capabilities use external tools". In fact `init` installs lean-ctx via a piped install script or Homebrew; browser automation can download mise, `agent-browser@latest` and Chrome for Testing on first use (opt out with `AGENTFLARE_BROWSER_NO_AUTO_INSTALL=1`); release builds download an ONNX embedding model on first semantic search; agent execution needs the agent and its credentials. No `crt-static` setting was found, so "static" is unverified. | `src/tool_install.rs:44-53`; `src/browser_install.rs:1-26`; `crates/flare-browser/src/lib.rs:33`; `ARCHITECTURE.md` "Installation and first use" | Site hero, footer and meta description: "The core CLI needs no Node." Disclose first-use downloads (section 5.10). |
| 7 | Medium | **The installer's checksum guarantee is conditional.** The Unix installer verifies SHA-256 only when `sha256sum` or `shasum` exists; otherwise it prints a warning and continues (`install.sh:109`). It does fail closed when `SHA256SUMS` cannot be fetched or does not list the file. README was corrected by #970; the site still says "checksum-verified". My earlier draft said "refuses to install unverified" without this qualifier. | `install.sh:95-175`; `ARCHITECTURE.md` "Checksums are conditional" | Site: "verifies its SHA-256 checksum when a checksum tool is available". |
| 8 | Medium | **License contradiction, now in three places.** Site hero and footer say MIT. `LICENSE` and `Cargo.toml` say Apache-2.0. The release workflow's generated Homebrew formula and Scoop manifest also hard-code `MIT` (`release.yml:199,287`), so the package metadata users see disagrees with the repo. | `LICENSE`; `Cargo.toml:14`; `.github/workflows/release.yml:199,287`; `ARCHITECTURE.md` "Resolved contradiction" | Apache-2.0 everywhere; fix the two literals in `release.yml`. |
| 9 | Medium | **The one-liner on agentflare.dev serves a v1.3.1 installer.** `site/src/worker.ts:18` pins `INSTALL_REF = "v1.3.1"`; current release is 1.8.1. The hero uses this endpoint; the install section uses `raw.githubusercontent.com/.../master`. | `site/src/worker.ts:18-19`; `site/public/index.html` | Bump the pin each release. **Until then all copy below uses the raw GitHub URL**, which is what the README documents. |
| 10 | Medium | **Stale version strings.** Footer says v1.4.0; current is 1.8.1. Scoop manifest is 1.8.1; winget manifest is 1.1.0. | `site/public/index.html` footer; `bucket/agentflare.json:2`; `winget/getappz.agentflare.yaml:2` | No version in copy; link to Releases. winget omitted from install copy. |
| 11 | Medium | **The site omits most of 1.5–1.8** and "no daemon required" is half true. Missing: supervisor, agent routing and failover, plan-approval and command-approval gates, durable workflows and schedules, bubblewrap sandbox, encrypted SQLite, Telegram approvals, browser automation, `pm`, `insights`, `apps`. Stdio MCP works without the daemon; schedules, workers, recovery and supervisor ticks need the daemon/dashboard process. | `CHANGELOG.md` 1.8.1; `ARCHITECTURE.md` "Background execution belongs to the dashboard/daemon process" | New Coordinate section (5.7). |
| 12 | Medium | **Sandbox and autostart are not cross-platform.** Process isolation is Linux bubblewrap only; on other platforms `try_wrap` runs the plain command unless a fail-closed policy is set. Daemon autostart installs on macOS and Linux only. | `crates/flare-sandbox/src/lib.rs:99`; `src/daemon_autostart.rs:4`; `ARCHITECTURE.md` "Concrete limitations" | Say "sandbox: Linux" and "autostart: macOS, Linux". |
| 13 | Low | **`KNOWN_ISSUES.md` was stale.** Fixed by #970: it now carries a "historical audit" banner and points to `ARCHITECTURE.md`. | `KNOWN_ISSUES.md:3-11` | Resolved. Still do not link it as current. |
| 14 | Low | **`lean-ctx` is linked under two owners.** README and installer use `yvgude/lean-ctx`; `compare.md` links `getappz/lean-ctx`. | `README.md`; `docs-site/.../compare.md` | Use `yvgude/lean-ctx` (what the installer downloads). |
| 15 | Low | **Version number vs maturity.** `STATUS.md` says Beta and that 1.x is not a stability claim. | `STATUS.md` "Versioning" | Keep "Beta" visible in nav and hero. |

Not found: any telemetry or analytics client (see P1). Resolved by #970 and no longer a finding: the README's "28-member workspace" (now 32 packages), the "no unsigned prebuilt binary" Windows wording, and "nothing is created if it already exists" for `init`.

---

## 2. Evidence base

### 2.1 Measurement types (never mix these)

| Type | Meaning | Where we have a number |
|---|---|---|
| Tool-output compression | Fewer tokens in what reads, searches and shell commands return to the agent | lean-ctx (upstream) |
| Input-file compression | Smaller memory/rule/doc files the agent loads | `optimize output` (no number verified) |
| Output-token reduction | The model writes fewer tokens | None verified for agentflare. The 65% figure comes from the upstream Caveman project. |
| Generated-code reduction | Less code written for the same task | `optimize code` (upstream Ponytail, n=4, Haiku 4.5) |
| Monetary savings | Dollars | Only lean-ctx's self-reported ledger, one user. Not generalizable. |
| Execution time | Wall-clock | Ponytail row ("~27% faster") only |

### 2.2 Claims register

"Public?" means suitable for marketing copy as written in the last column.

| ID | Claim | Source | Conditions and limits | Public? | Wording to use |
|---|---|---|---|---|---|
| C1 | Historical lean-ctx read-mode percentages | [Upstream measurement status](https://github.com/yvgude/lean-ctx/blob/main/BENCHMARKS.md) | Withdrawn by upstream; not current evidence, even with attribution. | No | Describe focused reads, searches and shell output. Ask readers to measure their repository with `lean-ctx benchmark report .`. |
| C2 | One maintainer's lean-ctx usage ledger | README "Real usage" | One maintainer and project; no controlled comparison or independently checked monetary result. | No | Omit the anecdotal savings figures. |
| C3 | `optimize output`: 65% average output-token reduction (22–87%, 10 prompts) | README; upstream Caveman | The command compresses a file; the figure is about model output under a terse-style prompt. No evals in this repo. Upstream doc warns of ~1–1.5k input tokens of overhead per turn that can make it net-negative on terse work. | No | Do not quote. Describe the command. |
| C4 | `optimize code`: ~54% less code, ~20% cheaper, ~27% faster | README; upstream Ponytail | 12 tasks, FastAPI + React repo, Haiku 4.5, n=4. Other models, languages, agents untested. Several `optimize code` subcommands print instructions for the agent; they do not themselves audit or measure (`ARCHITECTURE.md`). | With heavy qualifiers, secondary placement | "In the upstream project's benchmark (12 feature tasks, one FastAPI + React repo, Haiku 4.5), agents wrote about 54% less code. Your results will vary." |
| C5 | Built-in memory persists decisions across sessions | `STATUS.md` (stable); `src/memory/`; `tests/memory_cli.rs` | Local SQLite (`memory/brain.db`). Keyword recall always works; hybrid (vector) recall needs a `semantic` build and the downloaded model. Hybrid weights 0.4 BM25 / 0.6 vector, 30-day decay, MMR 0.7 (`src/memory/search.rs`). No effectiveness benchmark. | Yes (capability only) | "Remembers decisions between sessions." |
| C6 | `agentflare cost` reports spend from session transcripts | `src/cost.rs`; `src/rollup.rs`; `data/anthropic-pricing.json` | Reads Claude Code transcripts under `~/.claude/projects/`. Bundled price table can go stale. `insights` ingests Claude, Codex and OpenCode only (`crates/flare-insights`). | Yes, scoped | "See what your Claude Code sessions cost." |
| C7 | Work items and claims stop two agents taking the same task | `item`, `claim`; `src/mcp_server/item.rs:1068` | Claims have a liveness/TTL lifecycle. Worktree isolation and sandboxing are separate guarantees. Schema may change (STATUS). | Yes, beta | "Claim an item before you work it." |
| C8 | Git shim blocks writes to the default branch from the wrong worktree | `crates/agentflare-shim`, `flare-git-shim`; hook order in `src/hook.rs:467` (redirect, then claim guard, then enforced coaching rule) | Guardrail, not a security boundary: depends on the shim being on PATH and the host's hook support. | Yes, scoped | "A guardrail against accidental commits to your default branch." |
| C9 | Review verifies citations against the diff, merges duplicates, tags findings | `src/review.rs`; `flare-consensus` | Does not prove a finding correct. Criteria without evidence cannot keep a pass verdict. Schema not finalized. | Yes, capability only | "Checks that each finding points at a real line in the diff." |
| C10 | `agentflare work` and the supervisor run items through implement, review, judge and finalize roles in a worktree and open a PR | `src/cli/work.rs`; `src/work_item_pipeline.rs:482`; `ARCHITECTURE.md` | Beta. Needs an installed, authenticated agent. Bounded to 5 fix rounds and 50 tasks. A PR is not guaranteed to merge. AI agents are directed to `item redispatch` instead of calling `work`. Fixed in 1.8.x: process storms, duplicate PRs, worktree wipes. | Yes, labelled beta | "Hand a work item to a headless agent and get a pull request back. Beta." |
| C11 | Search: one MCP tool, many sources | `src/mcp_server/search.rs` | `all` = store + memory + code; `all+web` adds web. Source types: social, news, GitHub, academic, datasets, websites, weather, financial, crypto, FX, indicators, YouTube, Bluesky. External availability not tested. "17" taken from the README, not recounted. | Yes, with network note | "One search tool across local and public sources." |
| C12 | Encrypted credential storage | `src/auth_crypt.rs`: AES-256-GCM, PBKDF2-HMAC-SHA256, 600,000 iterations; `flare-vault`; SQLCipher option (CHANGELOG #824) | Auth profiles and the project vault are two separate formats. Clearing a local profile does not revoke a remote credential. | Yes, no superlatives | "Credentials are stored encrypted on your machine." |
| C13 | Local-first state | `ARCHITECTURE.md` storage table: `backend.db`, `agentflare.db`, `store.db`, `memory/brain.db`, `workflows.db`, `flare-docs.db`, `vault.json` | Applies to persistence only. Many features use the network (P3). | Yes, restricted to persistence | "State is stored in local files and SQLite databases." |
| C14 | Prebuilt binaries: Linux GNU x86_64/aarch64, macOS x86_64/aarch64, Windows MSVC x86_64 | `.github/workflows/release.yml:15-31` | No musl or Windows ARM build. Linux ARM uses `cross`. `install.sh` handles Linux and macOS only. Windows binary is not Authenticode-signed. | Yes | Section 6. |
| C15 | Release binaries carry cosign signatures and SLSA provenance | README "Verifying release binaries"; `release.yml` | Release workflow inspected, not exercised; signatures are not auto-verified by the installers. | Yes, with a link to the verify steps | "Signed with cosign; SLSA provenance attached." |
| C16 | Supported agents | `src/init.rs`, `src/components.rs`, `crates/agent-registry` | Depth differs (section 6.4). Registry also knows Gemini CLI and Copilot CLI for launch and routing. | Yes, via the matrix | Section 6.4. |
| C17 | Core CLI needs no Node | README (post-#970) | True of the CLI and hooks. Not of lean-ctx install, browser sidecar, or agent execution. | Yes, as worded | "The core CLI needs no Node." |
| C18 | Failover and routing between agents | CHANGELOG #380, #533, #539, #674; `docs-site/.../cli.md` (`[router]`, `[failover]`) | Config-driven; thresholds configurable; usage-based failover for Claude and opencode-go. Not exercised here. | Yes, labelled beta | "Optionally route work to another installed agent when one is rate-limited." |

### 2.3 Privacy, network and security: what the code supports

| ID | Statement | Support | Caveat |
|---|---|---|---|
| P1 | We found no telemetry or analytics client in the source. | Search for telemetry/posthog/sentry/mixpanel/segment found nothing relevant. HTTP clients: `ureq` (main binary, `agentflare-backend`, `flare-channels`, `flare-docs`), `reqwest` with rustls (`flare-proxy`), `rmcp` HTTP client (remote MCP servers). Every hard-coded host maps to a feature the user invokes. "Analytics" appears only in the local cost database (`analytics.db`). `ARCHITECTURE.md` independently notes "no production telemetry service is required by these local paths". | A source search is not a guarantee. Phrase as what we looked for. |
| P2 | Persistent state is stored locally. | `ARCHITECTURE.md` storage table; `crates/agentflare-config` | Platform cache and data directories are not all under `~/.agentflare`. |
| P3 | Network use is feature-driven: GitHub, Telegram/Slack/Discord, search sources, docs lookups (docs.rs/npm/PyPI), Claude usage lookup (`api.anthropic.com/api/oauth/usage`), Cloudflare AI gateway, model providers via agents or `flare-proxy`, remote MCP servers via the gateway, and any LLM-backed command. | `src/channels.rs`, `src/claude_usage.rs`, `src/mcp_server/search.rs`, `crates/flare-proxy`, `crates/gateway-registry` | Update checks run only when you run `agentflare update` (`check_for_update` has one caller, `src/update/mod.rs:127`). |
| P4 | Your agent still talks to its own model provider as before. agentflare adds no model to that path. | Architecture | Exceptions when enabled: `flare-proxy` (mounted at `/proxy/v1/messages`, token gate only if `AGENTFLARE_PROXY_TOKEN` is set), `optimize output` (LLM call), `decide` layer. |
| P5 | `serve` and `mcp --http` listen on loopback by default (`127.0.0.1:35273` and `127.0.0.1:35274`) but have **no authentication** and, for `serve`, **accept write and execute requests**. | `src/cli/serve.rs`; `src/cli/mcp.rs`; `ARCHITECTURE.md` "HTTP access" | Copy must not call the dashboard read-only. `--yes-expose` acknowledges non-loopback exposure; it adds no access control. |
| P6 | The Unix installer verifies SHA-256 when a checksum tool is present, and fails closed if `SHA256SUMS` is missing or does not list the file. | `install.sh:95-175` | Warns and continues when neither `sha256sum` nor `shasum` exists. `AGENTFLARE_SKIP_VERIFY=1` bypasses. |
| P7 | `init` modifies your agent's config (hooks, MCP, rules), may refresh managed rules, and may install lean-ctx. | `src/init.rs`; `src/components.rs`; `ARCHITECTURE.md` | Some steps prompt unless `-y`. Existing entries are merged, not only skipped. |
| P8 | `agentflare uninstall` removes config that `init` wrote; `install.sh --uninstall` removes only the binary. | `src/cli/mod.rs:211`; `install.sh:233` | |
| P9 | Process sandboxing exists on Linux only. | `crates/flare-sandbox/src/lib.rs:99` | Elsewhere jobs run unsandboxed unless fail-closed policy is set. |
| P10 | Credentials: auth profiles use AES-256-GCM with PBKDF2; the project vault uses `flare-vault`. | `src/auth_crypt.rs:7`; `src/vault.rs:3` | Two formats; an empty passphrase failure mode was recorded in the historical audit and was not revalidated. |

### 2.4 Things we must not claim

- Any customer, testimonial, user count, star count or download number. None exist in the repo.
- A combined savings percentage, or dollars saved for a typical user.
- That agentflare makes Codex, Cursor, Windsurf, Cline or Continue cheaper by a measured amount. All benchmarks used Claude Code.
- That the dashboard is read-only, or authenticated.
- "Hands-off", "production-ready", "enterprise" or "secure by default".
- Roadmap items. Epics and specs in the repo are not public commitments.

---

## 3. Positioning

### 3.1 Audience and job

**Primary audience:** developers who use a terminal or IDE coding agent every day (Claude Code first, then Codex, Cursor, opencode) and feel two kinds of friction: sessions that burn context and money re-reading things, and a second agent or session that steps on the first.

**Most important job:** "Let me keep working with my agent for longer without paying for the same context twice, and without losing what we decided."

**Secondary job (adopt later):** "Let me run more than one agent on a repo without them colliding."

Not the audience for now: teams that want a hosted, multi-seat PM tool with a browser UI. `compare.md` already says so honestly.

### 3.2 Category and value proposition

**Category:** a local toolkit that sits beside your coding agent ("agent toolkit"; not "platform" or "orchestrator").

**Value proposition:** agentflare makes coding agents cheaper to run, remembers what you decided between sessions, and, when you are ready, keeps several agents from colliding on the same repo.

### 3.3 Differentiators (each with evidence)

1. **Works with the agent you already use.** One `init` command for eight agents. (C16)
2. **Two layers, adopted in order.** Optimization works on day one for one agent; coordination is opt-in. (STATUS, README)
3. **Local persistence.** Memory, items, claims and workflow state live in SQLite on your machine. (C13, P2)
4. **Honest numbers.** Every figure is attributed, scoped to what it measured, and shown with its limits. (README Metrics approach)
5. **Visible guardrails.** Branch guard, plan-approval and command-approval gates, pre-handoff secret scan, signed releases. (C8, C15, CHANGELOG 1.8.1)

Do not lead with the feature count. `ARCHITECTURE.md` lists dozens of capabilities; the homepage teaches three ideas and links to the rest.

### 3.4 Explaining two layers without overwhelming

Frame, repeated on every page: **"Make one agent cheaper. Then make several agents safe."**

- Stage 1, *Optimize*: five capabilities (tool-output compression, memory, cost view, code-minimalism rules, prose compression for your own files).
- Stage 2, *Coordinate*: introduced by a concrete situation ("two agents, one repo"), then claims, review, handoffs. A beta row, "Automation", holds `work`, the supervisor and schedules.
- Everything else (search, docs lookup, vault, vent, proxy, apps, browser, insights) is one line plus a link to the Architecture page.

Terms are defined where first used: MCP, hook, worktree, claim.

### 3.5 Primary conversion

**Install and wire one agent** (installer, then `agentflare init --agent <yours>`). Secondary: read the docs; view the source. No signup or waitlist exists, so none is invented.

### 3.6 Voice

"impeccable.style" was not defined anywhere in the repo or this session. It is interpreted as a bar of precision and polish. If it refers to a specific style guide, supply it and the copy will be re-checked against it.

The bar: plain developer English, concrete nouns, short sentences, no superlatives, no emoji, no exclamation marks, every number with its unit and source, and the qualifier in the same sentence as the claim.

---

## 4. Site structure and navigation

**Top navigation:** `agentflare` (logo, with a "Beta" badge linking to STATUS) · Optimize · Coordinate · Numbers · Install · Docs · GitHub · **Install** (button)

**Homepage sections, in order:**

1. Hero
2. Compatibility strip
3. The problem
4. How it fits together
5. Optimize
6. Coordinate
7. A realistic first hour
8. Numbers
9. Local-first, and what leaves your machine
10. Install and first use
11. FAQ
12. Closing CTA
13. Footer

**Supporting pages:** Getting started · Concepts · CLI · MCP tools · Guides · Compare · **Architecture** (new; publish `ARCHITECTURE.md`, the source-cited reference added in #970) · **Status** (new; mirrors `STATUS.md`) · **Security and verification** (new; mirrors the README verification section).

The homepage stat band is removed. Its four figures mix four different measurements, which the Numbers section separates.

---

## 5. Final copy

### 5.1 Hero options

**A — Recommended**
- Headline: **Make your coding agent cheaper to run. Make several of them safe to run together.**
- Subheadline: agentflare is a command-line toolkit that trims what your agent reads, remembers what you decided between sessions, and keeps multiple agents from editing the same work. It works with the agent you already use.

**B**
- Headline: **Keep your coding agent focused, and keep your agents out of each other's way.**
- Subheadline: Compress tool output, remember decisions across sessions, and share a backlog and branch guard between agents. Open source, beta.

**C**
- Headline: **The toolkit that sits beside Claude Code, Codex and Cursor.**
- Subheadline: Fewer wasted tokens in one session. Fewer collisions across many. Wired in with one command.

**Recommendation: A.** It names both jobs in the order a newcomer needs them, contains no number in the headline, and makes no claim that needs a footnote. B is softer and less specific. C leads with brand names and reads as a compatibility list.

### 5.2 Hero block (final)

> **Eyebrow:** Open source · Apache-2.0 · Beta
>
> **H1:** Make your coding agent cheaper to run. Make several of them safe to run together.
>
> **Sub:** agentflare is a command-line toolkit that trims what your agent reads, remembers what you decided between sessions, and keeps multiple agents from editing the same work. It works with the agent you already use.
>
> **Command:** `curl -fsSL https://raw.githubusercontent.com/getappz/agentflare/master/install.sh | sh`
> **Then:** `agentflare init --agent claude-code`
>
> **Primary button:** Install agentflare   **Secondary button:** Read the docs
>
> **Microcopy under buttons:** Linux, macOS and Windows. Free and open source. `init` edits your agent's config files, and `agentflare uninstall` reverses that. Beta: see what's stable.

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
> **Layer 1: Optimize (works with one agent).** Compress what the agent reads, remember what you decided, see what it costs.
>
> **Layer 2: Coordinate (when you run more than one).** A shared backlog, claims so two agents do not take the same task, review of each other's work, and handoffs with context. Opt in tool by tool.
>
> **Footnote:** MCP is the standard way agents call external tools. A hook is a command your agent runs at set moments, such as session start. Both layers use MCP; hooks add automatic behavior where the agent supports them.

### 5.6 Optimize

> **Eyebrow:** Layer 1
> **Heading:** Spend fewer tokens on things the agent has already seen.
> **Intro:** These pieces work for a single agent. `agentflare init` sets up the first two for you.

**Card 1 — Compressed tool output (lean-ctx)**
Reads, searches and shell output can return focused views with references for recovering detail. agentflare installs lean-ctx, a separate open-source tool, and wires it into your agent. Measure your repository with `lean-ctx benchmark report .`; upstream has withdrawn its earlier read-mode figures.

**Card 2 — Memory across sessions**
Decisions, facts and preferences are saved to a local database and recalled in the next session, so you stop re-explaining the project. Search it with `agentflare memory search "<query>"`.

**Card 3 — Cost view**
`agentflare cost` totals what your Claude Code sessions have cost, by project and day, from the transcripts already on your disk. Claude Code only.

**Card 4 — Code-minimalism rules (`optimize code`)**
Adds a short set of instructions, delivered by hooks in Claude Code and Codex, that nudge the agent toward the smallest change that works. Based on the Ponytail project. Its benchmark measured how much code the agent wrote, not whether the code was better, so review diffs as you always do.

**Card 5 — Prose compression for your own files (`optimize output`)**
`agentflare optimize output compress <file.md>` rewrites a markdown file, such as a long rules or notes file, in a terser style so the agent reads fewer tokens each time it loads it. A language model does the rewrite, so it needs model access. `agentflare optimize retrieve <id>` brings back the original text that the command registered.

> **Under the cards:** Each piece saves a different thing, so we do not add the savings together.

### 5.7 Coordinate

> **Eyebrow:** Layer 2 · Beta
> **Heading:** When two agents work on one repo, give them a shared picture.
> **Intro:** A local backend, exposed over MCP, that any MCP-capable agent can use. No account is required, and its state is stored in local databases.

**Situation:** You ask one agent to fix the login bug and another to add rate limiting. Both open `auth.rs`.

**What agentflare adds:**

**Work items and claims.** A shared backlog on your machine. An agent claims an item before starting, and a second agent sees it is taken.

**Worktrees.** A claimed item gets its own checkout of the repo, so parallel agents do not overwrite each other's files.

**Branch guard.** A thin wrapper around `git` that refuses writes to your default branch from the wrong checkout. It is a guardrail against mistakes, not a security boundary.

**Review.** Agents submit findings against a diff. agentflare checks that each finding points at a real line, merges duplicates, and labels each CONFIRMED, UNIQUE, DISPUTED or UNVERIFIED.

**Artifacts and handoffs.** Specs and plans are saved as versioned pages that outlive the session. A handoff passes an item and its context to another agent, in a thread you can read.

**Automation (beta)**
`agentflare work` and the background supervisor take an item through implement, review and judge roles in its own worktree, then open a pull request. You can require a human decision first: plan approval before work starts, command approval, and approval over Telegram. Schedules can run a saved workflow on an interval. This needs the background process (`agentflare daemon start`, or `agentflare serve`), an installed and signed-in agent, and, for process sandboxing, Linux.
*Automation is the newest part of agentflare. Its flags and on-disk formats can change between releases. Try it on a repository you can afford to experiment on, and read the status page first.*

**Local dashboard.** `agentflare serve` shows boards, items, costs, claims and jobs in your browser. It is also an execution host: it can submit and control jobs and start queued work, and it has no login. Keep the default `127.0.0.1` binding.

**Also in the box,** documented on the Architecture page: a multi-source search tool, third-party API docs lookup, an encrypted credential vault, friction logging (`vent`), browser automation, session insights, and an optional Anthropic-compatible proxy.

### 5.8 A realistic first hour

> **Heading:** What the first hour looks like.
> **Note:** Commands exist in the current CLI. Output is described, not pasted, because it varies by project.

1. **Install and wire your agent.** Run the install command, then `agentflare init --agent claude-code`. `init` checks each component, installs lean-ctx if it is missing, merges hooks and the MCP server into your agent's config, and may offer to refresh managed rule files. Running it again is safe.
2. **Restart the agent** so it reads the new config. File reads and shell output now pass through lean-ctx.
3. **Ask the agent to remember something.** For example: "We use SQLite, not Postgres, for local state." In a later session, `agentflare memory context` shows what would be recalled.
4. **Check the bill.** `agentflare cost` shows spend per project from your Claude Code transcripts.
5. **Later, add a second agent.** Create two work items through the `item` MCP tool, let each agent claim one, and watch the claims with `agentflare claim`. `agentflare serve` adds a browser view; read the dashboard note above first.
6. **Undo anything.** `agentflare uninstall` removes the hooks, rules and MCP entries `init` added.

### 5.9 Numbers

> **Eyebrow:** Numbers
> **Heading:** Every figure here was measured by someone, on something specific.
> **Intro:** We list who ran each benchmark, what it measured, and where it stops applying. We do not combine them, and none is a prediction for your project. agentflare has not re-run them.

| What it measures | Result | Who measured it and how | Limits |
|---|---|---|---|
| **Generated code volume** (`optimize code`) | About 54% less code on average across 12 feature tasks; about 20% lower cost and 27% faster in the same runs | [The Ponytail project's benchmark](https://github.com/DietrichGebert/ponytail/blob/main/benchmarks/results/2026-06-18-agentic.md): FastAPI + React repository, Claude Haiku 4.5, 4 runs. | One repository, one model, small sample. It measures how much code was written, not whether it was better. |

> **Not included on purpose:** an output-token figure for `optimize output`. The command compresses files you give it, and no measurement of that effect has been published.
>
> **Check your own:** `lean-ctx gain` · `agentflare cost` · `agentflare memory context`

lean-ctx percentages are omitted because [upstream withdrew the historical figures](https://github.com/yvgude/lean-ctx/blob/main/BENCHMARKS.md). Its local representation reports do not establish answer quality or monetary savings.

### 5.10 Local-first, and what leaves your machine

> **Heading:** State stays local. Many features still use the network.
>
> **Stored on your machine.** Memory, work items, claims, review findings, artifacts, workflow state and the credential vault are kept in local SQLite databases and files.
>
> **Network use depends on what you turn on.** GitHub operations; Telegram, Slack and Discord notifications; the search tool and its public data sources; documentation lookups from docs.rs, npm and PyPI; remote MCP servers you register; any command that calls a language model; and your agent's own traffic to its model provider. Update checks happen only when you run `agentflare update`.
>
> **First-use downloads.** `init` installs lean-ctx, a separate tool, if it is missing. Release builds fetch a small embedding model the first time semantic memory search runs, and fall back to keyword search if that fails. The browser tool can download mise, `agent-browser` and Chrome for Testing unless you set `AGENTFLARE_BROWSER_NO_AUTO_INSTALL=1`.
>
> **The dashboard has no login.** `agentflare serve` and `agentflare mcp --http` listen on `127.0.0.1` by default. Do not expose them to a network you do not control.
>
> **Verify what you download.** The Unix installer checks the release's SHA-256 checksum when `sha256sum` or `shasum` is available, and stops if the checksum file is missing or does not match. Release binaries are signed with cosign and carry SLSA provenance; the steps are in the docs.
>
> Telemetry sentence omitted; see the decision in section 10.

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
| An agent | Any in the table below. agentflare does not include one and does not supply a model. |
| OS | Linux (x86_64, aarch64), macOS (x86_64, Apple Silicon), Windows (x86_64) |
| Install script | `curl` and `tar`; `sha256sum` or `shasum` for checksum verification |
| lean-ctx (installed by `init`) | `curl`, or Homebrew |
| Building from source | Rust (toolchain pinned in `rust-toolchain.toml`) and a working native linker |
| Automation (`work`, schedules) | A running daemon or `serve`, plus an installed, authenticated agent. Process sandboxing: Linux only. |
| Daemon autostart | macOS and Linux |

### 6.2 Install

**Linux and macOS**
```bash
curl -fsSL https://raw.githubusercontent.com/getappz/agentflare/master/install.sh | sh
```
Downloads the release for your platform. Verifies its SHA-256 checksum when `sha256sum` or `shasum` is available; if neither exists it warns and continues. Installs to `~/.local/bin` (set `AGENTFLARE_INSTALL_DIR` to change it). Run from inside a clone, it builds from source instead.

**Homebrew (macOS, Linux)**
```bash
brew tap getappz/agentflare
brew install agentflare
```

**Windows, Scoop (prebuilt)**
```powershell
scoop bucket add agentflare https://github.com/getappz/agentflare
scoop install agentflare
```
The Windows binary is not Authenticode-signed, so SmartScreen or antivirus may warn. You can verify it with cosign instead.

**Windows, from source** (needs Rust and a working native linker)
```powershell
git clone https://github.com/getappz/agentflare
cd agentflare
.\install.ps1
```

**Any platform with Rust**
```bash
cargo install --git https://github.com/getappz/agentflare
```
Source builds do not include the `semantic` feature that release binaries enable; keyword memory search still works. Build with `--features semantic` to match release behavior.

Check it worked: `agentflare --version`

### 6.3 First use

```bash
agentflare init --agent claude-code     # or codex, cursor, opencode, windsurf, vscode-copilot, cline, continue
```
Restart the agent afterwards. `init` checks components, installs missing ones, merges host configuration and may prompt to refresh rules; pass `-y` to skip prompts. Re-running is safe.

Then try:
```bash
agentflare doctor            # check the install and wiring
agentflare cost              # Claude Code spend, from local transcripts
agentflare memory context    # what memory would recall now
```

Optional, once you run more than one agent: `agentflare serve` for the local dashboard (it can also execute queued work; keep the default binding).

Remove it:
```bash
agentflare uninstall         # removes the hooks, rules and MCP entries init wrote
# then remove the binary, e.g. install.sh --uninstall
```

### 6.4 Compatibility matrix

Derived from `src/init.rs` and `src/components.rs`, and cross-checked against `ARCHITECTURE.md`. "Hooks" are commands the agent runs at set moments. "MCP" registers agentflare's tools with the agent. † = read from code, not run on a clean machine.

| Agent | `init --agent` | Rules file | Hooks | MCP tools | Notes |
|---|---|---|---|---|---|
| Claude Code | `claude-code` | Yes | Yes | Yes | Most tested. The only agent with `agentflare cost`; all published benchmarks used it. |
| Codex | `codex` | Yes | Yes (`~/.codex/hooks.json`, written by `init`) | Yes (`codex mcp add flare`) | Use `init` or the plugin, not both: both setups execute the same hooks (measured below). Codex's native Bash hook omits the exit status, so it cannot prove a test passed. |
| Cursor | `cursor` | Yes | Yes† (`.cursor/hooks.json`) | Yes | |
| opencode | `opencode` | Yes | Yes† (plugin) | Yes | Rules wired through `opencode.jsonc`. |
| Windsurf | `windsurf` | Yes | No | Yes | |
| VS Code (Copilot) | `vscode-copilot` | Yes (`.github/copilot-instructions.md`) | No | Yes (`.vscode/mcp.json`) | Project-level. |
| Cline | `cline` | Yes (`.clinerules`) | Plugin† | Yes | |
| Continue | `continue` | No | No | Yes (`.continue/mcpServers`) | MCP only. |

*Also known to `agentflare agents` for launch and routing, but not wired by `init`: Gemini CLI and Copilot CLI. Session insights ingest Claude, Codex and OpenCode only.*

---

## 7. FAQ

**Do I need more than one agent to benefit?**
No. Compressed tool output, memory and the cost view help a single agent. Coordination is for when you run more than one.

**Will it change my agent's files?**
Yes. `init` merges hooks, an MCP entry and rule files into your agent's configuration, may refresh rule files it manages, and installs lean-ctx if missing. `agentflare uninstall` removes what it added.

**Does it send my code or prompts anywhere new?**
agentflare adds no model to your agent's path, so prompts go to the provider your agent already uses. Features that call other services (GitHub, search sources, chat notifications, docs lookups, update checks you run, language-model commands, remote MCP servers) do so only when you use them. The list is in "Local-first".

**Does it cost anything?**
agentflare is free and open source under Apache-2.0. Features that call a language model use your own provider account.

**How much will I save?**
We cannot say. The published figures come from specific benchmarks run by upstream projects (see Numbers). Measure your own with `lean-ctx gain` and `agentflare cost`.

**Is it stable?**
It is beta. Optimization, memory and `init`/`update`/`uninstall` are listed as stable in STATUS. Work items, review, artifacts, handoffs and the MCP tool surface can change between releases, so pin a version before scripting against them.

**Does it work on Windows?**
Yes: a prebuilt x86_64 binary through Scoop, or a build from source. Process sandboxing for automated jobs and daemon autostart are not available on Windows.

**Is the dashboard safe to leave running?**
Treat it as a local service with no login. It listens on `127.0.0.1` by default and can submit and control jobs. Do not bind it to a shared network, and note that `--yes-expose` only acknowledges the risk.

**What needs the background process?**
MCP tools and most CLI commands work without it. Schedules, queued work, workflow recovery and the supervisor run in the daemon or `serve` process.

**Can the automation merge things without me?**
The supervisor can watch checks and, if configured, merge. Plan approval and command approval exist so you can require a human decision first. Treat automated dispatch as beta and keep approval on until you trust the setup.

**What if it breaks my agent's config?**
Back up the file first if you are cautious. `init` merges rather than overwrites, and `uninstall` removes only agentflare's entries. Report problems on GitHub.

**How is this different from running lean-ctx alone?**
lean-ctx covers tool-output compression and is a smaller install. agentflare installs and wires it, then adds memory, cost reporting and the coordination layer. If you only want compression, use lean-ctx alone.

**How is this different from Linear or Jira?**
Those are hosted, multi-person tools. agentflare's tracker lives on your machine, needs no account, and is built for agents to read and write. It does not replace a team's shared planning tool.

**Who builds it?**
The getappz organization on GitHub. Source, issues and releases are public.

---

## 8. CTA labels, microcopy, footer

### 8.1 CTA labels

| Where | Label | Microcopy |
|---|---|---|
| Nav button | Install | |
| Hero primary | Install agentflare | Linux, macOS, Windows. Free, open source, Apache-2.0. |
| Hero secondary | Read the docs | Getting started |
| Copy button | Copy | (toast) Copied |
| Optimize section | See how to set it up | |
| Coordinate section | Read the coordination guide | Beta: flags and formats may change. |
| Numbers section | How these were measured | Links to the upstream sources |
| Install section | Copy install command | Verifies a checksum when a checksum tool is available. |
| FAQ | Ask a question on GitHub | |
| Closing | Install agentflare · View on GitHub | |

### 8.2 Page metadata

- **Title:** agentflare: a toolkit for coding agents
- **Meta description:** An open-source command-line toolkit that compresses what your coding agent reads, remembers decisions across sessions, and coordinates several agents on one repo. Works with Claude Code, Codex, Cursor, opencode and more.
- **Open Graph title:** agentflare: make your coding agent cheaper to run
- **Open Graph description:** Wired in with one command. Beta.

### 8.3 Footer

> **agentflare** — A toolkit for coding agents. The core CLI needs no Node.
>
> **Product:** Optimize · Coordinate · Numbers · Install
> **Resources:** Docs · CLI reference · MCP tools · Architecture · Compare · Status (what's stable) · Changelog
> **Trust:** Security policy · Verify a release · Apache-2.0 license
> **Community:** GitHub · Issues · Contributing
>
> © 2026 agentflare. Apache-2.0. Beta software: see Status before depending on any specific flag or tool name.
>
> *Do not print a version number in the footer. Link "Changelog".*

---

## 9. Appendix: current site content to change

| Current text on `site/public/index.html` or docs | Problem | Replace with |
|---|---|---|
| "Open source · Rust · MIT · Beta" and "MIT License" | License is Apache-2.0 | Apache-2.0 |
| "v1.4.0 · beta · local-first · daemon optional" | Stale (1.8.1); "local-first" is unqualified | Drop version; keep "Beta" |
| Stat band: 98.1% / 65% / 54% / 34.2M | Four measurements in one row; 65% mislabelled; 34.2M is one user | Remove; use the Numbers table |
| "optimize output — Conversation verbosity compression, ~65% fewer tokens" | The command compresses markdown files | Card 5 (5.6) |
| "Companion tool… up to 99%" | Historical read-mode figures were withdrawn by upstream | Capability description without a percentage; link the measurement status |
| "Committed evals"; "Confidence: High" column | No such evals; self-assigned rating | "The upstream project's benchmark"; show Limits |
| "A single static binary — the only runtime dependency is agentflare itself" | lean-ctx, browser downloads, ONNX model, agent and credentials | "The core CLI needs no Node" and 5.10 |
| "Every host above reads hook config directly into its own settings" | Four of eight agents have no hook wiring | Compatibility matrix (6.4) |
| "✓ rules written · lean-ctx installed · hooks + MCP wired · memory ready" | Simulated output that claims hooks for hosts that get none | Describe, or show real per-agent output |
| "Codex is the one exception: it needs its own plugin-loader step" | Superseded by `init` since `ea7dfa99` | "Run `agentflare init --agent codex`" |
| "Linux / macOS — prebuilt, checksum-verified" | Verification is conditional | "Verifies a checksum when a checksum tool is available" |
| "No daemon required" (Coordinate intro) | Schedules, workers, supervisor need it | "Core tools need no daemon; automation does" |
| "optional background daemon plus a read-only dashboard" | Dashboard executes work, no auth | "local dashboard and execution host, no login" |
| "vault & proxy… flare-proxy" | Proxy is mounted by the dashboard at `/proxy/v1/messages`; configured separately from agent setup | Move to the Architecture page |
| Hero uses `agentflare.dev/install.sh`; install block uses raw GitHub | Worker pinned to v1.3.1 | One URL; bump `INSTALL_REF` |
| "Works with VS Code" | Integration targets Copilot in VS Code | "VS Code (Copilot)" |
| "Run coding agents lean. Coordinate a whole crew of them." | Overreach; automation is beta | Hero A (5.1) |
| `docs-site/.../cli.md:28-31`, `getting-started.md:40-44` | Plugin-first Codex instructions | `init` alone |
| `src/cli/serve.rs:3` help text "read-only" | Router accepts POST and controls | "Serve the agentflare dashboard and execution host" |
| `release.yml:199,287` license "MIT" | Disagrees with `LICENSE` | "Apache-2.0" |

---

## 10. Prerequisite decisions and remaining publication checks

The three prerequisites for item #780 are resolved before the site/docs fix round:

- **A — Codex double-firing confirmed.** Codex CLI 0.164.0 on Windows, isolated `AGENTFLARE_HOME_OVERRIDE` and `CODEX_HOME`, with actual `agentflare init --agent codex -y`. A temporary catalog in the [documented local-marketplace format](https://developers.openai.com/codex/plugins/build) installed copies of this checkout's `.codex-plugin/plugin.json` and `hooks/hooks.json`. Windows command overrides counted handler starts and delegated the original commands with unchanged stdin. The two cases ran sequentially in separate Codex processes, using a closed localhost model endpoint and `--dangerously-bypass-hook-trust` for the reviewed test hooks only. No model completion was needed. Assertions checked the runtime counts below. The site and setup docs therefore make init and the plugin exclusive; prefer init alone.

  | Event | init alone | init + plugin |
  |---|---|---|
  | SessionStart | 2 handlers | 4 handlers (2 init + 2 plugin) |
  | UserPromptSubmit | 2 handlers | 4 handlers (2 init + 2 plugin) |

  Each source contributes the general agentflare handler and the optimize-code handler. These are executed starts, not configured-entry counts. Model connection failure was expected; the runtime probes were terminated after the counts were captured. Other lifecycle events and platforms were not exercised. Temporary files and the init-added Windows User PATH entry were removed.
- **B — Strike the telemetry sentence.** Omit the optional claim from public copy, consistent with PR #975's recorded omission. This is a copy decision, not an additional source audit or guarantee.
- **C — Ponytail source supplied.** [2026-06-18 agentic benchmark](https://github.com/DietrichGebert/ponytail/blob/main/benchmarks/results/2026-06-18-agentic.md): 12 feature tasks, FastAPI + React, Haiku 4.5, four runs per task/arm; about 54% less code, 20% lower cost, 27% faster. These remain upstream measurements, not an agentflare rerun.

Remaining checks and product decisions:

1. **One-liner.** `INSTALL_REF` is v1.8.1 (also returned by `gh release view`); the homepage uses `agentflare.dev/install.sh` throughout. Bump the pin on future releases.
2. **Dashboard.** Decide whether `serve` should stay a combined dashboard and execution host or gain authentication; the current copy states the risk plainly.
3. **Windows signing.** The copy discloses the unsigned binary; signing would let it drop that line.
4. **Not run on a clean machine:** `init` for Cursor, opencode and Cline (†), the Homebrew and Scoop installs, and the cosign/SLSA verification steps.
