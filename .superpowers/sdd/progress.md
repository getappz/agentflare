Plan: 2026-07-23-flare-docs-implementation-plan (artifact iI-eMBHI6YNsDp9e4g-Bo)
Branch: task/325

(Previous content here was stale leftover from an unrelated, already-merged
feature (gateway-search-execute, commits 68cd5dd..a8da060) — reset before
starting this plan's execution.)

Task 1: complete (4aee28c4..d013ec10, review approved, no Critical/Important.
Minor (plan-inherited, for final-review triage): open_default() discards
create_dir_all errors via `let _`; no test exercises open_file/default_db_path/
open_default (tempfile dev-dep unused until a later task); get() takes no
PROJECT_ID scoping — brief-mandated signature, doc_get's safety in a shared
global store unverifiable from this diff alone.)

Task 2: complete (d013ec10..00b9eae6, review approved, no Critical/Important).
Went through THREE design iterations, all human-directed, not implementer
churn:
  1. wreq (b1a2076c, original brief's choice) — abandoned: transitively
     requires BoringSSL via boring-sys2 (NASM + matching CMake/VS generator
     needed just to compile on Windows), and debug-profile `cargo test`
     failed to LINK (MSVCRTD/_CrtDbgReport CRT mismatch, boring-sys2's CMake
     invocation defaults to Debug which conflicts with Rust's always-release-
     CRT linking) — only --release worked. Flagged to the human as a Risk to
     escalate by both this session's controller and the Task-2 reviewer
     independently.
  2. reqwest+rustls-tls (cd53ee78) — human decided to swap off wreq for
     Phase 1 (fingerprint-evasion was for FUTURE non-Rust HTML scraping, not
     needed for docs.rs's plain JSON API). Fixed the debug-link issue
     (Cargo.lock 490->448 packages, whole boring-sys2/tokio-boring2/NASM tree
     gone). Still async (tokio+async-trait).
  3. ureq (00b9eae6, FINAL) — human pointed out ureq is this repo's actual
     house-standard HTTP client (6+ existing crates: agentflare-store,
     agentflare-backend, flare-output, gateway-registry, skill-registry, root
     binary), synchronous/blocking, no native-toolchain dependency at all.
     Fetcher trait made synchronous (dropped tokio+async-trait entirely from
     flare-docs); UreqFetcher mirrors the real pattern already in
     crates/agentflare-store/src/embedding_pipeline/download.rs. Scratchpad
     plan file updated in place (Tasks 3/4/5 code blocks + Tech
     Stack/Architecture sections) to reflect sync fetch_and_store and drop
     the tokio::task::block_in_place/tokio::runtime::Runtime bridges that
     were only needed for an async Fetcher — Task 4/5 briefs not yet
     generated at time of this edit, so no rework needed there.
Reviewer independently verified (not just trusting reports): cargo test -p
flare-docs passes 3/3 in debug profile with zero toolchain workaround, cargo
tree -p flare-docs shows zero reqwest/wreq/tokio/async-trait in the crate's
dependency graph, no workspace=true introduced, real ureq 2.x API used
correctly (status/header/body extraction verified against actual source).
Minor (plan-inherited, for final-review triage): fetch.rs's `!(200..300).
contains(&status)` check is likely dead code since ureq's .call() already
errors on non-2xx by default — belt-and-suspenders, not wrong, worth a
comment; no dedicated UreqFetcher unit test (brief-mandated deferral to Task
3's integration test — Task 3 reviewer should confirm that coverage lands).

Task 3: complete (00b9eae6..87e01431, review approved, no Critical/Important).
docs.rs JSON resolver: docs_rs_json_url, extract_root_docstring, sync
fetch_and_store tying Fetcher+decompress_zstd+DocsStore together, all through
DocsStore's PROJECT_ID="global" facade (never a raw agentflare_store::Store
call) — confirmed no async/tokio/async_trait leakage anywhere. One deviation
(agentflare_store::documents::{Document, DocUpsertOpts} import path instead
of brief's flat agentflare_store::{...}) verified necessary and consistent
with Task 1's already-established lib.rs re-export pattern. 8/8 tests pass.
Task 3's own fake-fetcher integration test confirmed real (only the network
boundary is faked; zstd/JSON-parse/DocsStore/FTS all run for real).
Minor (plan-inherited): store_raw_json_blob is a boilerplate 1-line wrapper
(brief-mandated); RustdocError::InvalidJson stringifies serde_json::Error
instead of #[from]-wrapping it (brief-mandated simplification).

Task 4: complete (87e01431..3c407e50, review approved, no Critical/Important).
flare_docs MCP tool (search|get|list|refresh) wired into AgentflareMcp:
flare_docs_store/flare_docs_store_override fields + ensure_flare_docs_store/
with_flare_docs_store helpers mirror store/ensure_store/with_store exactly
(confirmed against the real pre-existing pair) while entirely separate from
it; FlareDocsRequest matches MemoryRequest's house style field-by-field; no
async bridge anywhere (fetch_and_store called synchronously, per Task 2's
ureq pivot); diff to mcp_server.rs confirmed surgical (mod line inserted
alphabetically, no unrelated reformatting). One real deviation: `use` ->
`pub(crate) use` in flare_docs.rs to fix mod flare_docs shadowing the
flare_docs extern-prelude entry — correctly diagnosed root cause, not a
workaround. 2/2 tests pass, using :memory: override (never touches real
~/.agentflare/flare-docs.db). Reviewer noted the brief's own "Interfaces"
line still says WreqFetcher (stale pre-pivot text, not an implementation
defect - brief-authoring artifact only).
Minor (plan-inherited): get/refresh arms near-identical boilerplate
(verbatim brief code, follow-up could extract a serialize() helper).

Task 5: complete (3c407e50..0f8bc70b, review approved, no Critical/Important).
`agentflare docs search|get|list|refresh` CLI subcommand, thin wrapper over
the same flare_docs API Task 4's MCP tool uses. No async runtime anywhere
(fetch_and_store called directly, sync). mod docs; inserted alphabetically;
Docs variant appended at enum end matching the file's real append-newest-
at-end convention (not alphabetical there, correctly not forced to be); free-
function docs::run(cmd) dispatch matches vent/about/git's pattern as
directed. Commit-message backtick-mangling incident caught+fixed via amend
before anything else, diff unaffected (0f8bc70b is the correct final commit).
IMPORTANT DISCOVERY (not a Task 5 defect — Task 5's job was to surface it,
which it did correctly by propagating the crate error via eprintln!+exit(1)):
manual live-network verification (`docs get serde`) failed with "invalid
rustdoc json: missing \"root\" field" — Task 3's extract_root_docstring was
tested only against a synthetic string-typed root id fixture ("0:0"); the
REAL docs.rs payload (format_version 60) has root as a JSON NUMBER (e.g.
3177), with index keyed by the stringified number. Logged via vent (event
d6a8cb6a). Controller independently reproduced by fetching+decompressing the
real serde/latest payload via a throwaway (deleted, uncommitted) example
using flare-docs's own decompress_zstd+serde_json — confirmed root cause
exactly. Dispatched a fix task to crates/flare-docs/src/rustdoc.rs (accept
root as either string or number, stringify for index lookup) before the
final whole-branch review, since this is core happy-path functionality
(fetching real crate docs), not cosmetic.
Minor (plan-inherited): Get's --help text says "or read from cache" but
always re-fetches (TTL/cache-check explicitly deferred); print_or_die-style
error-handling boilerplate repeated per arm (brief-mandated verbatim code).

Task 3 FIX (numeric root id): complete (0f8bc70b..e3ced028, review approved,
no Critical/Important). extract_root_docstring now discriminates on the raw
serde_json::Value for "root": Value::String kept as-is (no regression for
existing string-root fixtures), Value::Number stringified before the index
lookup (matches real docs.rs payload: root=3177 (bare number), index keyed
by "3177"), any other type -> descriptive InvalidJson error, no panic path.
9/9 tests pass, full workspace cargo build clean, LIVE cargo run -- docs get
serde --version latest confirmed working end-to-end (real serde docstring
returned). Diff scoped strictly to rustdoc.rs as directed.
Minor: error message uses {other:?} Debug-dumps the whole Value for
unexpected root types (could be verbose for Object/Array, not observed in
practice); n.to_string() assumes integer JSON number, would mismatch if root
were ever serialized as a float (theoretical, not observed in real payload).

ALL 5 TASKS + 1 CROSS-TASK FIX COMPLETE. Full history: 4aee28c4 (base) ->
d013ec10 (T1) -> b1a2076c/cd53ee78/00b9eae6 (T2, 3 design iterations:
wreq->reqwest->ureq, human-directed) -> 87e01431 (T3) -> 3c407e50 (T4) ->
0f8bc70b (T5) -> e3ced028 (T3 numeric-root-id fix, discovered via T5's live
network smoke test).

FINAL WHOLE-BRANCH REVIEW (opus, d503df01..e3ced028 = merge-base..HEAD, note
this range also contains 2 unrelated already-merged PRs (#313/#314) at its
base -- actual feature range is 4aee28c4..e3ced028): "Ready with fixes".
Both cross-task properties independently verified by tracing source (not
trusting the ledger): (1) PROJECT_ID="global" enforcement airtight -- every
Document read/write funnels through DocsStore, no caller anywhere touches
agentflare_store::Store's doc methods directly or opens the shared store.db;
(2) sync pipeline confirmed zero async/tokio/await leakage in the crate.
ONE Important finding (real, invisible to any single task's diff): the
blocking ureq fetch inside the sync #[tool] fn flare_docs runs INLINE on the
MCP server's single-threaded (new_current_thread) tokio runtime -- rmcp's
`#[tool]` macro only Box::pins async fns, so a sync fn's body (including a
blocking network call) executes directly on the runtime thread with no
spawn_blocking anywhere in rmcp's dispatch tree. A get/refresh freezes the
WHOLE MCP server (transport, other tool calls, cancellation) for the fetch
duration (up to ~330s on a hung socket per the configured timeouts), and the
std::sync::Mutex guard is held across the blocking call too, serializing
concurrent flare_docs requests behind it. Fix: make flare_docs async, run
the network fetch via tokio::task::spawn_blocking (NOT block_in_place --
panics on current-thread runtimes), never hold the store mutex across an
.await. Dispatching ONE fix for this per skill guidance (Critical+Important
get fix dispatches; Minor goes to ledger for human triage, not auto-fixed).

Minor findings (not auto-fixed, for human triage before merge):
1. 404/bad-package-name maps to internal_error not invalid_params in get/
   refresh (src/mcp_server/flare_docs.rs) -- doesn't mirror the
   gateway_execute/skill_load caller-mistake-vs-infra-failure discrimination
   pattern already established elsewhere in this file.
2. FlareDocsRequest.limit / CLI --limit unbounded (MemoryRequest documents
   "max 50" by contrast).
3. decompress_zstd/read_to_end have no output-size cap (theoretical hardening
   against a compromised/oversized docs.rs payload; docs.rs is trusted today).
4. CLI docs.rs: Get's --help says "or read from cache" but Get/Refresh share
   one arm and both always re-fetch (TTL/cache-check explicitly deferred,
   ledger-acknowledged from Task 5's own review already).
5. fetch.rs's `!(200..300).contains(&status)` check is dead code (ureq's
   .call() already errors non-2xx) -- ledger-noted from Task 2's review.
6. docs_rs_json_url hard-codes host so no SSRF/traversal surface, but ureq
   follows redirects by default -- crate-name charset validation would close
   even the theoretical redirect-amplification surface (defense-in-depth
   only, not a real vuln today).

FINAL-REVIEW FIX (83f76add): blocking-runtime hazard fixed and re-reviewed,
approved, no Critical/Important. flare_docs tool method made async, network
fetch isolated to tokio::task::spawn_blocking using only an owned
UreqFetcher+url (no self/store borrow crosses the spawn boundary -- no
lifetime/Send hacks needed). fetch_and_store split into itself (unchanged
signature/behavior, confirmed via CLI's zero-diff + 9/9 crate tests still
passing) + new store_fetched (decompress/parse/store, called synchronously
AFTER the spawn_blocking().await resolves -- traced: no std::sync::MutexGuard
ever spans an .await). crates/flare-docs stayed fully sync (zero tokio in its
Cargo.toml); search/list/get-by-id untouched; src/cli/docs.rs zero diff.
JoinError (task panic) mapped distinctly from fetch error, not swallowed.
744-test full workspace run + live MCP stdio concurrency smoke test (fast
`list` calls return before a slow `get` completes) both independently
reproduced by the reviewer (9/9 flare-docs, 2/2 flare_docs:: MCP-layer).
Minor (not auto-fixed): the concurrency/non-freezing property has no
committed regression test, only the ad-hoc (gitignored, uncommitted)
mcp_smoke.py script -- a future regression wouldn't be caught by `cargo
test`; get-by-id's `if req.id.is_some() { req.id.expect(...) }` restructure
is a slightly indirect way to consume an Option vs `if let Some(id)`.

ALL WORK COMPLETE: 5 tasks + 2 cross-task fixes (numeric-root-id parsing,
blocking-runtime hazard), every task and fix individually reviewed and
approved, final whole-branch review completed with its one Important finding
fixed and re-approved. Full commit range: 4aee28c4..83f76add (feature scope;
merge-base d503df01 also includes 2 unrelated already-merged PRs #313/#314
at its base, not part of this feature). Next: superpowers:finishing-a-
development-branch.
