# claude-obsidian feature audit vs agentflare

Date: 2026-10-25 · Branch: `task/774-audit-claude-obsidian-features-vs-agentf`

Scope: `AgriciDaniel/claude-obsidian` v3.0.1 (MIT), cloned at
`.refs/claude-obsidian` (depth 1) at upstream HEAD
`2e545563172f97f02ecaaaab376ceea1246a0583` (2026-10-22). Compared against agentflare's current
optimization + coordination surface, with a per-capability verdict of
**have / partial / absent**, and a ranked list of what is worth copying,
adapting, or deliberately skipping.

Method: read the upstream README, AGENTS.md, WIKI.md, docs, all 15 SKILL.md
files, the Python core module docstrings, hooks/contracts/adapters config, and
the retrieval/eval scripts; then checked each overlapping capability in
agentflare with `rg`/`Select-String` plus direct reads of `src/` and `crates/`.
Upstream claims are cited by file; agentflare claims by `path:line`.

---

## 1. Verdict

claude-obsidian is not a competing agent harness. It is a **single-purpose,
file-based knowledge system**: a local Obsidian vault, a stdlib-Python
transaction core, 15 Agent Skills, and 3 read-only subagents. Its value to
agentflare is not "another feature" but **five reusable mechanisms** that
agentflare currently lacks or only approximates:

1. A real multi-file, recoverable **workspace transaction** (precondition
   hashes → inspect → hash-bound approval → journaled apply → rollback/recover).
2. A **provenance ledger model** that separates source evidence from claim
   assessment (authority, independence, freshness, contradiction).
3. A separate **semantic-completeness gate** (`workflow_complete`) on top of
   storage validity, enforced in the same operation.
4. **Deterministic Markdown corpus linting** (link resolution, orphans,
   duplicate basenames, frontmatter policy, empty sections, stale index).
5. A **retrieval quality harness** with labeled queries, negative cases,
   Recall@k/MRR, and hash-bound inputs — applied to content, not just skills.

agentflare is already ahead on: cross-host support and packaging, single-binary
distribution, hybrid retrieval math (BM25 + vectors + RRF + decay + MMR),
memory/session infrastructure, multi-agent coordination (items/claims/handoff),
durable workflow journals + schedules, GitHub ops, and the approval gate for
commands. Nothing in claude-obsidian's *coordination* or *host integration*
layer is worth replacing those.

### Top findings

| # | Finding | Value | Effort |
|---|---------|-------|--------|
| 1 | **No multi-file user-workspace transaction.** agentflare has per-file crash-atomic writes (`src/atomic_fs.rs:1-11`), WAL SQLite (`src/db.rs:108-110`), a workflow journal (`crates/flare-workflow/src/engine.rs:6`), and a command approval gate (`crates/agentflare-approval/src/lib.rs:1-10`) — but no primitive that plans N file writes against expected hashes, journals originals, applies atomically, and rolls back the whole set. claude-obsidian's `claude_obsidian/transaction.py` is exactly that. | High | M–L |
| 2 | **No source/claim provenance ledger.** agentflare memory has execution-backed lessons (`README.md` "Evidence-backed project lessons"), relations, and confidence, but no source record model with `authority`/`independence_key`/`refresh_due`/review state, and no falsifiable-claim record with `accepted|provisional|contested|unsupported|deprecated` semantics (`claude_obsidian/ledgers.py:28-41`, `skills/wiki/references/provenance.md`). | High | M |
| 3 | **No retrieval-quality evaluation for store/memory.** `skill eval` already reports Hit@1/Hit@3/MRR/nDCG (`src/cli/skill.rs:455`), but nothing scores `agentflare-store` chunks or memory recall with labeled queries, negative cases, and snapshot hashing. claude-obsidian ships a complete read-only benchmark (`docs/retrieval-evaluation.md`, `scripts/evaluate-retrieval.py`, `tests/fixtures/retrieval-quality/`). | High | S–M |
| 4 | **No Markdown corpus lint.** No wikilink/link parser, orphan detection, duplicate-basename detection, page frontmatter schema, empty-section check, or stale-index check exists in `src/` or `crates/`. The only frontmatter parsers are for skills and `flare-output` (`crates/skill-registry/src/frontmatter.rs`, `crates/flare-output/src/frontmatter.rs`). claude-obsidian's `lint_engine.py` is 85 KB of deterministic, write-free checks. | Medium–High | M |
| 5 | **Storage validity is not separated from knowledge completeness.** agentflare gates item claims on plans (`src/mcp_server.rs:1706`) and completion on verification evidence, but an artifact/doc write can succeed while omitting the index/log/hot-context coupling claude-obsidian's `workflow.py` enforces. | Medium | S–M |
| 6 | **No contextual chunk prefixes.** `agentflare-store` chunks with `text-splitter` (`crates/agentflare-store/src/chunk.rs:1-6`) and can embed, but never prepends a situating prefix before indexing. claude-obsidian implements Anthropic's contextual-retrieval ingest side with a deterministic default (`scripts/contextual-prefix.py:1-30`). | Medium | S |
| 7 | **Capture is attachment-only.** Agentflare has content-addressed blobs and assets (`src/asset_store.rs:1`, `crates/agentflare-store/src/blobs.rs`), but no inbox, adapter-maturity manifest, capture budgets, immutable raw archive, or URL-credential/redirect safety (`config/adapters.json`, `claude_obsidian/capture.py`, `url_safety.py`). | Medium | M–L |
| 8 | **Capabilities are checked imperatively, not declared.** `doctor`/`verify` inspect wiring and completion (`src/doctor.rs:1`, `src/verify.rs:1`); there is no tracked contract per capability with `available|configured|verified|degraded` and a verification command (`config/capabilities.json`, `claude_obsidian/contracts.py:26-40`). | Medium | M |
| 9 | **Bounded hot-context injection.** agentflare's SessionStart injects a briefing/context (`src/hook.rs:50-110`); claude-obsidian caps hot context at 500 words / 32 KiB, wraps it in a `trust="local-data" instructions="never"` envelope, and makes injection consent-gated (`hook_adapter.py:24-33`, `AGENTS.md`). | Low–Medium | S |
| 10 | **Obsidian product surface** (Canvas, Bases, Obsidian Markdown, CLI transport, defuddle, LYT/PARA/Zettelkasten modes, marketplace/plugin packaging) is product-specific and should be skipped. | — | — |

---

## 2. What claude-obsidian actually ships

### 2.1 The product

- **Model**: drop sources into `inbox/`, keep immutable copies in `.raw/`,
  generate linked, source-cited pages under `wiki/`, query read-only, save only
  on explicit request, lint deterministically (`README.md`, `WIKI.md`).
- **Layout**: `inbox/`, `.raw/`, `wiki/{sources,entities,concepts,questions,canvases,meta/ledgers}`,
  `wiki/{index,log,hot,overview}.md`, `.vault-meta/` for runtime state
  (`WIKI.md` "Root layout").
- **Core**: Python 3.11 stdlib package `claude_obsidian/` (~1.1 MB across
  `transaction.py`, `capture.py`, `checkpoint.py`, `ledgers.py`, `lint_engine.py`,
  `contracts.py`, `release.py`) plus `scripts/` for retrieval/eval/mode helpers.
- **Skills**: 15 (`skills/*/SKILL.md`) — 5 core workflows, 7 extensions,
  3 reference skills.
- **Agents**: 3 read-only subagents (`agents/verifier.md`,
  `agents/wiki-ingest.md`, `agents/wiki-lint.md`). Only the transaction core
  writes.
- **Hosts**: Claude Code plugin + Agent Skills for Codex, OpenCode, Gemini,
  ZCode, Cursor, Windsurf (`README.md`, `scripts/setup-multi-agent.sh`).

### 2.2 Mechanism inventory (what matters)

| Mechanism | Where | Contract |
|---|---|---|
| Vault resolution | `claude_obsidian/paths.py:389` | `--vault` → env → workspace config → unambiguous discovery; fail closed; never the product tree |
| Multi-file transaction | `claude_obsidian/transaction.py:1-7` | one process lock, precondition hashes, durable journal, atomic per-file replace, rollback/recover |
| Hash-bound approval | `transaction.py:1333`, refs | inspect materializes a plan; `approval_sha256` binds the expanded plan to the resolved vault root |
| Provenance ledgers | `ledgers.py:28-41`, `provenance.md` | source identity/authority/independence/freshness/review; claim assessment + contradictions |
| Workflow acceptance | `workflow.py`, `docs/workflow-acceptance.md` | `storage_valid` and `workflow_complete` reported separately; catalog/log/hot-cache coupling |
| Deterministic lint | `lint_engine.py:1-30` | dead/ambiguous links, orphans, duplicate basenames, frontmatter, empty sections, stale index, ledger violations; read-only, gitignore-aware |
| Retrieval | `bm25-index.py`, `retrieve.py`, `rerank.py` | Okapi BM25 (k1=1.5, b=0.75) over contextualized chunks; optional local Ollama cosine rerank with graceful no-op; explainable candidates |
| Contextual indexing | `contextual-prefix.py:1-30` | 2,000-char paragraph-aware chunks, 4,000 hard split, 200 overlap, ≤1,000-char prefix; deterministic synthetic prefix by default |
| Retrieval eval | `docs/retrieval-evaluation.md`, `scripts/evaluate-retrieval.py` | Recall@1/5/10, MRR, negative cases, stale filtering, input hashes, P50/P95 latency |
| Prompt eval | `docs/prompt-evaluation.md` | frozen cases + binding hash over skill text/fixtures; strict response schema |
| Capture | `capture.py`, `config/adapters.json` | adapter maturity (`implemented`/`metadata-only`/`external-runner-required`), budgets, content-addressed create-only raw bytes, queue |
| URL safety | `url_safety.py:1-20`, `capture.py:1046-1168` | credential-in-URL detection, public-host validation, redirect-chain validation |
| Capability contracts | `config/capabilities.json`, `contracts.py:26-40` | per-capability scope, needs, confirmation, verification command, state |
| Release discipline | `package_validation.py`, `release.py`, `SHA256SUMS` | package/contract/hook/manifest validation, allowlisted artifacts, deterministic build |
| Hooks | `hooks/hooks.json`, `hook_adapter.py` | only SessionStart + Stop; bounded hot-context injection, consent-gated |
| Modes | `mode_config.py`, `scripts/wiki-mode.py` | Generic/LYT/PARA/Zettelkasten filing; new notes only, never migrates |
| Checkpoints | `checkpoint.py` | explicit, exact-operation Git checkpoints, separate from transactions |
| Capacity/recovery | `docs/capacity-and-recovery.md` | ledger size caps, lossless compaction, recovery rules |

### 2.3 Notable domain extras

- **Boundary score** (`scripts/boundary-score.py`): `(out_degree − in_degree) ×
  recency` to find frontier pages for autoresearch.
- **Semantic tiling lint** (`scripts/tiling-check.py`): embedding-based
  duplicate-page candidates, localhost-only by default.
- **Log fold** (`skills/wiki-fold`): extractive rollup of operation history,
  never rewriting history.
- **Forward acceptance fixtures** (`tests/fixtures/forward/`): frozen
  ingest/query/save scenarios for fresh-context host testing.

---

## 3. Capability map

| Capability | claude-obsidian | agentflare today | Verdict |
|---|---|---|---|
| Single-binary distribution | Python + bash, requires 3.11 | Rust, no runtime deps | **agentflare ahead** |
| Cross-host support | 6 hosts via setup script | 20 agents, hooks + MCP | **agentflare ahead** |
| Multi-file recoverable transaction | Yes, core primitive | No (per-file atomic + WF journal only) | **absent → copy** |
| Crash-atomic single file | `atomic_write`, fsync dirs | `src/atomic_fs.rs` | **have** |
| Plan/approval before mutation | Hash-bound plan per operation | Command-class approval + item plan gate | **partial → adapt** |
| Source/claim provenance | Full ledger model | Execution-provenance lessons only | **partial → copy schema** |
| Contradiction preservation | First-class | Not modeled | **absent** |
| Markdown link graph / lint | Full deterministic engine | None | **absent → copy** |
| Page schema / vocabulary | Declared in one module | Skill frontmatter only | **absent** |
| BM25 retrieval | Sparse index over chunks | FTS5 doc + chunk BM25 | **have** |
| Hybrid / vector retrieval | Optional Ollama rerank | BM25 + vectors + RRF/merge/decay/MMR | **agentflare ahead** |
| Contextual prefixes | Yes | No | **absent → copy** |
| Retrieval evaluation | Recall@k/MRR harness | Skill eval only | **partial → generalize** |
| Doc version history | File history via Git/checkpoint | DB doc versions (`documents.rs:1136`) | **have (different)** |
| Artifact versioning | Notes + Git | Artifacts versions/diff/optimistic concurrency (`agentflare-artifacts/src/store.rs:174,373,394`) | **agentflare ahead** |
| Durable workflow + journal | No scheduler; transaction journal only | `flare-workflow` journal, WaitEvent, schedules | **agentflare ahead** |
| Multi-agent coordination | 3 read-only agents, single writer | Items/claims/handoff/review/daemon | **agentflare ahead** |
| Memory across sessions | `wiki/hot.md` + notes | SQLite FTS5 + vectors + sessions + sync | **agentflare ahead** |
| Bounded hot-context injection | 500 words / 32 KiB, trust-tagged, opt-in | SessionStart briefing; no explicit content cap | **partial → adopt caps** |
| Capture adapters + budgets | Matrix, budgets, immutable raw, queue | Asset attach + blobs | **partial → copy** |
| URL safety | Credential/redirect/host validation | No shared model | **partial → adopt** |
| Capability contracts | Declarative, verified/degraded | Imperative doctor/verify | **partial → adopt** |
| Package/release validation | Extensive, version-pinned | CI + `cargo-deny` + guard | **different product; skip** |
| Obsidian Canvas/Bases/CLI | Yes | No | **skip** |
| Methodology modes | Yes | No | **skip** |
| Git checkpoints | Explicit per operation | Git shim + PR flow + worktrees | **agentflare ahead** |

---

## 4. Deep dives on the five asymmetric gaps

### 4.1 Multi-file workspace transaction

**What claude-obsidian does.** One logical operation is one bundle:

1. Read every target, record `sha256` (or `null` for expected absence).
2. Parallel workers return drafts only; no shared writes.
3. Merge into one `claude-obsidian.transaction.v1` document.
4. `inspect` → validate, expand managed requests, compute
   `approval_sha256`.
5. `apply --approved-plan-sha256` → hold one process lock, journal originals,
   verify hashes, fsync + atomic replace per file.
6. Any failure/interruption → deterministic rollback or `transaction recover`.
7. Replay is idempotent only for the same operation ID + bytes; a changed
   target returns conflict exit 75.

Bounds are explicit: 64 MiB per file, 128 MiB per operation of new content or
recovery backups, 1,024 writes, 8 MiB journal/result (`operation-transactions.md`,
`docs/compound-vault-guide.md`). Confinement uses open directory descriptors, so
a vault-controlled symlink swap cannot redirect writes; Windows writes are
refused and require WSL.

**What agentflare has.** `src/atomic_fs.rs` gives crash-atomic single-file
writes with a read-only-directory fallback. SQLite gives ACID state for
items/claims/workflows/docs. `flare-workflow` journals step state and supports
`WaitEvent` human-in-the-loop (`crates/flare-workflow/src/engine.rs:6`,
`definition.rs:385`). `agentflare-approval` classifies commands and persists
pending approvals with a 10-minute fail-closed TTL
(`crates/agentflare-approval/src/lib.rs:1-10`).

**Gap.** None of these can safely update a *set* of user-owned files (e.g. an
exported knowledge tree, a generated docs site, a config bundle, multiple
artifacts written to disk) as one reviewable, recoverable unit. `atomic_fs`
protects one file; SQLite protects DB rows; the WF journal protects workflow
state. A crash between file 3 and 4 of a 10-file publish leaves a torn tree with
no recovery primitive.

**Recommendation.** Add a `workspace-transaction` crate (or extend
`atomic_fs`) with:

- `plan { ops: [{path, mode: create|replace|delete, content, expected_sha256}] }`
  → canonical JSON + `approval_sha256` bound to the resolved root.
- `apply { plan, approval_sha256 }` → single lock, write-ahead journal under a
  managed runtime dir, per-file temp+rename, fsync, rollback on error.
- `recover` → finish or roll back an interrupted operation.
- Enforce: N-write cap, per-file and aggregate byte caps, duplicate-path
  rejection, case-collision rejection, symlink/junction confinement.
- Expose as `workspace` CLI + MCP action, and reuse it inside artifact export,
  `apps` projection, and any future knowledge ingestion.

This is the highest-leverage copy because it is a **generic safety primitive**;
the Obsidian use case is just one consumer.

### 4.2 Provenance ledger model

**What claude-obsidian does.** Two ledgers, deliberately not overloaded:

- **Source ledger**: stable ID, SHA-256, vault-relative or HTTPS locator,
  `authority` (`official|primary|secondary|community|synthetic|unknown`),
  `independence_key`, retrieval/review timestamps, `refresh_due`, review state
  (`unreviewed|active|superseded|rejected`), linked pages.
- **Claim ledger**: falsifiable claim, note location, supporting/contradicting
  source IDs, confidence, risk, review state, assessment
  (`accepted|provisional|contested|unsupported|deprecated`).

Rules: accepted claims need a fresh, active, non-synthetic source; high-risk
accepted claims need two independent sources; sources sharing an
`independence_key` or canonical URL do not corroborate; contradictions are
preserved (`skills/wiki/references/provenance.md`).

**What agentflare has.** Memory observations + relations + lessons with
execution receipts. Promotion requires a terminal workflow run and evidence
observation IDs, and confidence is explicit (`README.md`). That is
*execution* provenance, not *source* provenance: it proves "this lesson came
from run X with observations Y", not "this statement is supported by two
independent primary sources, one of which is stale".

**Gap.** Artifacts, docs, memory lessons, and agent claims have no shared way to
say where a statement came from, how authoritative it is, whether sources are
independent, whether it contradicts another record, or whether it is stale.

**Recommendation.** Port the *schema*, not the storage: add optional metadata
to docs/artifacts/memory lessons:

- `sources[] {id, sha256, locator, authority, independence_key, retrieved_at, refresh_due, review}`
- `assessment {state, support[], contradict[], risk, confidence}`
- validation helper enforcing the acceptance rules.
- A `provenance` view/action that returns stale, contested, and unsupported
  records for lint/doctor.

Do **not** build a second SQLite store; extend `agentflare-store` documents
metadata (it already has tags/metadata/versions).

### 4.3 Workflow acceptance: storage vs meaning

claude-obsidian refuses to conflate "the write is valid" with "the knowledge
operation is complete". `workflow.py` inspects:
`storage_valid`, `workflow_complete`, findings, and `approval_sha256`.
Save must couple note + catalog + log + hot cache; ingest must additionally
provide a source and change at least one ledger; the log must preserve all prior
entries; a new MOC must be linked from an active catalog
(`docs/workflow-acceptance.md`).

agentflare's item plan gate (`submit_plan`/`approve_plan`, `plan_required`)
and completion gate are close in spirit but live at different layers and are
advisory/structural per item, not per write operation.

**Recommendation.** When adopting 4.1, add a pluggable acceptance hook:
`storage_valid` from the transaction engine, `workflow_complete` from a
per-workflow validator. Start with one real consumer (artifact publish or a
future knowledge ingest), not a generic DSL.

### 4.4 Markdown corpus lint

claude-obsidian's `lint_engine.py` is a deterministic, read-only engine that
parses a deliberately small navigation subset (wikilinks, embeds, inline path
links, heading/block fragments, code fences, comments, math) and reports:

- dangling and ambiguous links, duplicate basenames;
- orphan pages (no inbound links);
- missing/invalid frontmatter and unsupported values;
- empty sections;
- stale index entries;
- source/claim ledger contract violations;
- gitignore-aware resolution and allowlists.

agentflare has no equivalent. This matters if agentflare keeps growing
file-based knowledge surfaces (skills, artifacts on disk, docs exports, rules,
AGENTS.md trees). A broken link or orphaned page is exactly the class of silent
drift that `doctor` currently cannot see.

**Recommendation.** Implement a `corpus lint` action with a **narrow, declared
parser subset** (start: Markdown links + wikilinks + headings + frontmatter +
`.gitignore`), no rendering, no writes, stable sorted output. Reuse the pattern
of reporting machine-readable findings and keeping repair as a separate
transaction. Do not attempt full CommonMark/Obsidian parity — claude-obsidian
explicitly documents its subset and its tests (`tests/test_lint_engine.py` is
113 KB of cases worth mining for fixtures).

### 4.5 Retrieval evaluation

agentflare evaluates skill retrieval (`src/cli/skill.rs:455`) but not content
retrieval. claude-obsidian's harness is a useful template:

- query file with `id`, exact text, `expected_pages[]`, optional `category`;
- empty expected list = negative case, never inflates recall;
- metrics: Recall@1/5/10, MRR, no-result accuracy, usable-candidate recall;
- staleness filtering by page hash; malformed candidates counted separately;
- input/index hashes bind a run; P50/P95 latency;
- read-only, no model calls by default, explicit rebuild.

**Recommendation.** Generalize `skill eval` into a retrieval-eval command that
can target `agentflare-store` (and memory). Reuse the fixture design: invented
pages, multilingual/Unicode cases, an intentional lexical miss, stale/conflicting
records, and unanswerable queries. This also gives 4.4 and 4.6 a measurement
loop.

---

## 5. Ranked adoption plan

### P0 — do next (highest leverage, bounded scope)

1. **Workspace transaction primitive** (4.1).
   - New crate `workspace-tx` (or extend `atomic_fs`), CLI `agentflare workspace`, MCP action.
   - Journal under the repo/`.agentflare/tx/` (gitignored), recovery command.
   - Tests: torn-write simulation, conflict on changed target, symlink swap,
     cap enforcement, replay idempotency, recovery after interrupt.
   - First consumer: artifact export / apps projection (low blast radius).
2. **Retrieval eval for store + memory** (4.5).
   - Add labeled-query fixtures; assert floors in CI like skill eval.
   - No new deps: reuse FTS/BM25 and existing vector code.
3. **Corpus lint (narrow subset)** (4.4).
   - Start with `.claude/skills/**`, `docs/**`, `AGENTS.md` trees.
   - Output JSON findings; no writes; add `agentflare doctor --corpus`.
4. **Provenance schema on docs/artifacts/lessons** (4.2).
   - Metadata-only first; validation + stale/contested reporting second.

### P1 — next quarter

5. **Contextual chunk prefixes** in store ingestion.
   - Deterministic synthetic prefix by default; optional model prefix behind an
     explicit egress flag; cache keyed by `(model, scheme, chunk_hash)`.
   - Measure with P0 #2 before/after.
6. **Workflow acceptance hook** (4.3).
   - Separate `storage_valid` / `workflow_complete` in the workspace-transaction
     report; one real workflow validator first.
7. **Capture pipeline + URL safety** (2.2).
   - Adapter maturity manifest, budgets, content-addressed create-only raw
     archive, URL credential/redirect validation.
   - Reuse `openresearch`/`flare-docs` fetch paths rather than adding a new
     network stack.
8. **Bounded, trust-tagged context injection** (2.2 / 2.3).
   - Cap session context bytes; wrap external/local corpus text in a
     `trust="local-data" instructions="never"` envelope; keep opt-in for
     vault-like paths.

### P2 — opportunistic

9. **Capability contracts** (finding #8).
   - A small TOML/JSON file per capability with `needs`, `verification_command`,
     `configured|verified|degraded`, consumed by `doctor`.
   - Only worth it once the verification commands exist; otherwise it becomes
     documentation that lies.
10. **Forward acceptance fixtures**.
    - Port the idea of frozen ingest/query/save scenarios to agentflare's
      knowledge paths; keep synthetic data only.

### Explicitly skip

| Upstream feature | Why skip |
|---|---|
| Python core / stdlib packaging | agentflare is deliberately a single Rust binary (`README.md` "Why Rust, not Node") |
| WSL-only write model | Windows is a first-class agentflare host; descriptors/`flock` are not portable |
| Obsidian Canvas / Bases / Obsidian Markdown / wiki-cli / defuddle | Obsidian-specific product surface, no agentflare consumer |
| LYT / PARA / Zettelkasten modes | Filing policy for a note product; no equivalent domain |
| Claude Code marketplace/plugin packaging | agentflare already ships cross-host integrations and plugins |
| Git checkpoints | Git shim, worktrees, and PR flow cover this |
| Boundary score / semantic tiling | Useful only with a vault + autoresearch product; revisit if agentflare ships knowledge ingest |
| Read-only subagent trio | agentflare's item/claim/handoff model already separates drafts from writers |

---

## 6. Licensing and attribution

claude-obsidian is MIT; agentflare is Apache-2.0. MIT code can be included in
an Apache-2.0 project, but the MIT copyright/permission notice must be retained.
Recommended posture:

- **Prefer reimplementation of mechanisms** (schemas, invariants, test cases),
  which is what most of the list above needs.
- If any source file or fixture is copied verbatim, add it to `NOTICE` with the
  upstream copyright and MIT text, and record the commit SHA used.
- Do not copy the upstream test corpus wholesale without checking its
  provenance (the repo itself warns against copying third-party corpora:
  `docs/native-conformance.md`).
- The upstream tree is a `.refs/` clone and must not be committed.

---

## 7. Verification plan for adopted work

| Item | Evidence required |
|---|---|
| Workspace transaction | crash/interrupt recovery test; conflict test (target changed after inspect); symlink/junction confinement test; idempotent replay test; cap tests |
| Retrieval eval | checked-in labeled queries; CI floor assertions; negative-case accuracy reported separately |
| Corpus lint | fixture corpus with dangling/ambiguous/orphan/duplicate/frontmatter/empty-section cases; stable sorted JSON snapshot |
| Provenance | validation tests for acceptance rules (freshness, independence, synthetic, contradiction); stale/contested report |
| Contextual prefixes | before/after Recall@k + MRR on the same corpus and labels; deterministic-prefix snapshot |
| Capture/URL safety | credential-in-URL rejection; redirect escape rejection; budget enforcement; create-only raw immutability |
| Context injection | byte cap; trust envelope present; opt-in path required for external corpora |

Every item should follow the repo gate before push:

```bash
mise run verify
# or
cargo fmt --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings -A unsafe_code -A clippy::pedantic
cargo test --workspace
```

---

## 8. Appendix — upstream file map used in this audit

```text
.refs/claude-obsidian/
  claude_obsidian/
    transaction.py         multi-file journaled transactions (195 KB)
    ledgers.py             source + claim provenance contracts (55 KB)
    lint_engine.py         deterministic Markdown lint (85 KB)
    workflow.py            storage vs workflow-completeness gate (23 KB)
    capture.py             adapters, budgets, queue, content-addressed raw (85 KB)
    contracts.py           capability/product contracts (50 KB)
    checkpoint.py          explicit Git checkpoints (51 KB)
    paths.py               vault resolution + confinement (16 KB)
    url_safety.py          credential detection
    gitignore.py           deterministic .gitignore matching for lint
  scripts/
    contextual-prefix.py   contextual retrieval ingest side (64 KB)
    bm25-index.py          sparse BM25 index over chunks (32 KB)
    retrieve.py            hybrid retrieval orchestrator
    rerank.py              optional local Ollama cosine rerank
    evaluate-retrieval.py  Recall@k / MRR harness
    evaluate-prompts.py    frozen prompt-case harness
    boundary-score.py      frontier-page scoring
    tiling-check.py        embedding duplicate-page lint
  skills/                  15 SKILL.md workflows
  agents/                  3 read-only subagents
  config/
    capabilities.json      per-capability scope/needs/verification
    adapters.json          capture adapter maturity matrix
    product-contract.json  release/verification contract
  docs/
    retrieval-evaluation.md
    prompt-evaluation.md
    workflow-acceptance.md
    capacity-and-recovery.md
    compound-vault-guide.md
    native-conformance.md
    windows-wsl.md
  tests/                   ~900 KB of behavior/fixture tests
```
