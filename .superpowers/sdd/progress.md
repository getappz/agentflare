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

Task 1: complete (68cd5dd..df08c42, review approved — one Important finding
resolved by controller as a false positive: brief's "Interfaces" line used
gateway_registry::db:: as a fully-qualified-path label, not a public-API
contract; db stays a private mod, consumed intra-crate by later tasks, matching
the plan's own later lib.rs revisions)

Task 2: complete (df08c42..c3b3442, review approved, no Critical/Important.
Minor (plan-inherited, for final-review triage): search.rs FTS-operator test
only asserts no-error not no-false-match; schema_json parse failure silently
maps to Value::Null instead of surfacing an error.)

Task 3: complete (c3b3442..518c907, review approved, no Critical/Important.
IDE dead_code/"not in module tree" flags confirmed stale false alarms — mod
is wired in, pub-reachable items aren't flagged by lib-crate dead_code lint.
Minor (plan-inherited, for final-review triage): no test at exact distance=3
threshold boundary or distance=0 via suggest(); no Display/to_string test for
GatewayError variants.)

Task 4: complete (518c907..7b13955, review approved, no Critical/Important.
Minor (plan-inherited): HttpApi's optional fields (auth_ref/tools) not tested
in the omitted/default case, only McpStdio's defaults are covered.)

Task 5: complete (7b13955..0702b74, review approved, no Critical/Important/
Minor issues. No-McpStdio-variant constraint correctly respected.)

Task 6: complete (0702b74..b7a40c1, two commits: 49909e5 feat + b7a40c1 fix,
both reviewed and approved). Highest-risk task in the plan — real rmcp client
transport, verified by reviewers actually running the child-process-spawning
integration tests, not just trusting reports. Deviations from original brief
(all compiler-driven, vetted as correct):
  - discover() integration tests relocated to tests/mcp_stdio_discover.rs
    (CARGO_BIN_EXE_gateway-fixture-server is only set for integration-test/
    bench targets, not src/ unit tests) — same assertions, just relocated.
  - fixture_server.rs uses #[tokio::main(flavor = "current_thread")] (crate's
    tokio dep only enables "rt", not "rt-multi-thread") — matches src/main.rs's
    own Commands::Mcp runtime pattern.
  - Cargo.toml: rmcp (client+transport-child-process+server+transport-io+
    macros) and schemars moved from [dev-dependencies] to [dependencies] —
    the [[bin]] gateway-fixture-server target needs them and Cargo never
    exposes dev-dependencies to bin targets; previously only "worked" via
    accidental workspace-wide feature unification with the root crate's own
    rmcp dependency. tempfile correctly stays in [dev-dependencies] (only
    used in #[cfg(test)] code).
Task 7: complete (b7a40c1..de11d16, review approved, no Critical/Important.
McpStdioBackend::call() implemented over the real rmcp
client — CallToolRequestParams::new + .call_tool(), is_error/structured_content
handling, per the brief's Step 3 unchanged). Deviation (as directed, same
CARGO_BIN_EXE_gateway-fixture-server reason as Task 6): the two new call()
tests went into a new tests/mcp_stdio_call.rs integration-test file instead of
inline in src/mcp_stdio.rs's unit-test module. Both tests pass against the
real spawned fixture-server process; full crate suite (24 tests across lib +
2 integration files) green; `cargo build -p gateway-registry` standalone
still succeeds. Reviewer independently cross-checked every rmcp type/method/
field claim against the real vendored rmcp-1.8.0 source — no discrepancies.
Minor (plan-inherited, for final-review triage): no test for the non-object/
non-null args-validation branch; is_error message is raw serde_json content
dump rather than extracted text (both per brief's Step 3 verbatim).

Task 8: complete (de11d16..aac267c, review approved, no Critical/Important.
UTF-8 cut-point safety manually traced by reviewer and confirmed correct.
Minor (plan-inherited): _original_chars/_shown_chars are byte counts not char
counts despite field names; the UTF-8-boundary test's budget saturates to 0
before exercising a real mid-character cut, so it passes trivially rather than
proving the property (reviewer separately hand-verified the property holds).)

Task 9: complete (aac267c..8834012, two commits: 859b294 feat + 8834012 fix,
both reviewed and approved, second reviewer independently ran the tests).
Registry ties db+search+config+backends+debounced refresh together — the
main integration point. Deviations (all documented in-code, vetted correct):
  - The 4 (now 5) fixture-spawning tests relocated to tests/registry.rs, same
    CARGO_BIN_EXE_gateway-fixture-server reason as Tasks 6/7.
  - open_in_memory made a plain public method (not #[cfg(test)]-gated) since
    an external integration-test crate can't see test-gated items; mirrors
    db::open_in_memory's existing style.
  - HttpToolConfig needed #[derive(Clone)] added (build_backends's
    tools.clone() didn't compile without it — brief predated this field).
  - Real bug found+fixed: ensure_fresh's discovery loop used
    backend.discover().await? — one failing backend (e.g. a crashed
    mcp_stdio server, or simply configuring one http_api-kind server, which
    always fails discover() by design) aborted the WHOLE registry refresh,
    making every other healthy backend's tools unreachable too. Fixed to a
    per-backend match: failures are logged (eprintln!, no tracing dep in this
    crate) and skipped, db::rebuild still runs with whatever succeeded. New
    test one_failing_backend_does_not_block_the_others proves a healthy
    fixture backend stays searchable/executable despite a real, deterministic
    http_api failure alongside it — independently re-run and confirmed by the
    reviewer, not just trusted from the report.
Unrelated observation (out of scope, not touched): one pre-existing flaky
test, ponytail::config::tests::defaults_to_full, env-var pollution across
parallel test threads — confirmed passes in isolation, no ponytail files
touched by this plan.

Task 10: complete (8834012..aaedca3, review approved, no Critical/Important —
security-sensitive review of credential-handling code, wrong-passphrase path
traced end-to-end and confirmed correct, no silent-failure/empty-string
coercion anywhere). src/gateway_secrets.rs in the ROOT crate, reuses
auth_crypt as-is (untouched), no coupling to the unrelated auth.rs/auth_db.rs
OAuth-profile vault. Deviations: test command is `--bin agentflare` not
`--lib` (crate has no [lib] target); env::set_var/remove_var wrapped in
unsafe {} (required by edition 2024); reused the existing
agent_registry::detect::PATH_LOCK (same lock src/paths.rs and src/agents.rs
already use for env-var-mutating tests) rather than inventing a new one —
note paths.rs's own alias for this lock isn't re-exported, so the direct
import was the only way this could compile, and it does.
Minor (plan-inherited/cosmetic, for final-review triage): mod gateway_secrets;
landed in a different position in main.rs than the brief specified (harmless,
mod order doesn't matter in Rust); create_dir_all error discarded via `let _`
(inherited from brief); test cleanup on the wrong-passphrase env var isn't
panic-safe (pre-existing weakness pattern already in paths.rs's test_support).

Task 11: complete (aaedca3..fb689c0, review approved, no Critical/Important).
CLI subcommand `agentflare gateway secret set/list/remove`. Real, expected
deviation: the codebase's CLI dispatch had been refactored (unrelated prior
work) from a monolithic src/main.rs into src/cli/mod.rs + per-subcommand files
under src/cli/ (coaching.rs, auth.rs, etc.) by the time this task ran — added
src/cli/gateway.rs following that real sibling pattern (verified directly
against coaching.rs/auth.rs) instead of the brief's stale main.rs snippet;
command surface/behavior unchanged. stdin-only secret input (never a CLI arg)
confirmed correctly enforced. No automated tests expected for this task (CLI
plumbing over Task 10's already-tested functions) — verified via manual smoke
test, code-traced as plausible/consistent with real code paths.

Task 12: complete (fb689c0..1a98fbb, two commits: e067829 feat + 1a98fbb fix,
both reviewed and approved, both independently re-run by reviewers). FINAL
integration — gateway_search/gateway_execute wired into the real AgentflareMcp
MCP server. Full workspace suite green throughout (198 tests in the
agentflare bin suite alone, 0 failures anywhere). Deviations:
  - Root Cargo.toml's tokio needed "sync" feature added (gateway-registry's
    own Cargo.toml already had it).
  - Real bug found+fixed, reaching back into the already-approved Task 9 file:
    gateway_registry::Registry held conn: rusqlite::Connection directly;
    Connection is Send but not Sync (RefCell-based statement cache), so
    Registry wasn't Sync, so &Registry (borrowed from the tokio::sync::Mutex
    guard) wasn't Send, which rmcp's #[tool] macro requires for its boxed
    future. Fixed by wrapping conn in std::sync::Mutex<Connection> inside
    Registry, updating the 3 call sites (ensure_fresh/search/execute) —
    reviewer traced all three and confirmed the lock is never held across an
    .await anywhere, only for synchronous SQLite calls.
  - Test additions matched this file's actual existing idiom (tempdir()+
    path().join(), err.to_string().contains()) rather than the brief's
    slightly different assumed shape — confirmed genuine convention-following
    by direct comparison against pre-existing tests in the same file.
  - Real bug found+fixed: gateway_execute blanket-mapped all 6 GatewayError
    variants to invalid_params, even though 4 of them (NotImplemented,
    Connection, Upstream, Sqlite) are infrastructure failures, not caller
    mistakes — unlike skill_load's existing discrimination pattern it should
    mirror. Fixed to a 2-arm match (ServerNotFound/ToolNotFound ->
    invalid_params, else -> internal_error) with a new test proving the
    ServerNotFound path maps to the real ErrorData::INVALID_PARAMS code, not
    just an assertion on message text. Both reviewers independently re-ran
    the mcp_server test suite (14 then 15 passing) rather than trusting
    the reports.
Minor (for final-review triage): the new gateway_execute_unknown_server test
only overrides gateway_db_override, not the gateway.toml config path itself —
load_gateway_config() still reads the real ~/.agentflare/gateway.toml,
un-isolated (safe in practice since the test's server name won't collide with
anything real, but a pre-existing test-isolation gap shared by all the
gateway_search/gateway_execute tests, not introduced by either fix).

Task 13: complete. End-to-end manual smoke test against the REAL compiled
agentflare.exe binary (not cargo test) — controller ran this directly, no
subagent. Built gateway-fixture-server + agentflare, wrote a real gateway.toml
under a temp AGENTFLARE_HOME_OVERRIDE pointing at the fixture binary, drove
the real stdio MCP protocol by hand (initialize -> initialized ->
tools/call gateway_search -> tools/call gateway_execute). Full success:
initialize handshake completed; gateway_search("echo") found the fixture's
echo tool with full metadata (description, input_schema, BM25 score);
gateway_execute against it returned the real "echo: hello" result. One
environment snag along the way (not a code bug): first attempt used a
Git-Bash-style path (/c/Users/...) in gateway.toml's command field, which
native Windows CreateProcess doesn't understand ("path not found", os error
3) — this also happened to be a nice unplanned confirmation that Task 12's
error-discrimination fix works for real (the resulting Connection error
correctly surfaced as internal_error/-32603, not invalid_params). Fixed by
using a Windows-style path (C:/Users/...), re-ran, fully green.

ALL 13 TASKS COMPLETE. Full workspace test suite green throughout. Next:
final whole-branch review per superpowers:subagent-driven-development, then
superpowers:finishing-a-development-branch.

FINAL WHOLE-BRANCH REVIEW: Ready to merge "With fixes". Confirmed the two
highest-risk cross-task properties both hold (Backend enum dispatch is a
genuine extensibility seam; the secrets-to-spawned-process path really works
end to end, traced CLI set -> DB -> resolve_gateway_secrets ->
build_backends -> env injection -> spawn). Two genuine Important findings,
invisible from any single task's review:
  1. gateway_execute holds the whole-Registry tokio Mutex guard across an
     unbounded downstream .await (no timeout anywhere in the crate) — one
     hung/slow backend wedges EVERY server's gateway_search/gateway_execute,
     not just its own, until the call resolves or the process is killed.
  2. Secret-injection failures are silently swallowed across 3 layers: (a)
     build_backends only injects when BOTH auth_ref AND auth_env are set,
     but the design spec's OWN example config only sets auth_ref — a user
     following the design doc gets silent no-injection; (b)
     resolve_gateway_secrets's .ok().flatten() discards WrongPassphrase/
     NoPassphrase, indistinguishable from "no secret configured"; (c) a
     typo'd auth_ref hits the same silent path. All three surface only as a
     mystifying downstream auth failure with zero indication the gateway
     dropped the credential.
All ledger Minor findings triaged individually by the final reviewer — every
one confirmed as genuinely Minor (no live bugs found on independent
inspection), except Task 12's test-isolation gap which folds into Important
finding #2 above rather than standing alone.
Dispatched ONE consolidated fix subagent for both Important findings (per
skill guidance: one fix subagent for a final review's complete list, not
per-finding) — timeout added in mcp_stdio.rs (McpStdioBackend::discover/call)
mapping to a new GatewayError::Timeout variant; config.rs now validates
auth_ref/auth_env must be paired at parse time (rejects the design doc's own
ambiguous example instead of silently no-op'ing); resolve_gateway_secrets and
build_backends now log (eprintln!, matching the crate's existing no-tracing-
dependency style) when a configured secret fails to actually reach a spawned
backend's environment.

FINAL-REVIEW FIX (a8da060): the fix subagent stalled for a long time fighting
a real hang in its own new mcp_stdio_timeout.rs test. Controller took over
directly (subagent had genuinely reproduced a real bug, wasn't stuck for no
reason): the hung-backend test fixture (GATEWAY_FIXTURE_HANG=1, simulates a
downstream server that never completes the MCP handshake) became an orphaned
zombie process — tokio::process::Child does NOT kill the OS process on drop
unless kill_on_drop(true) is set on the Command, so the client-side
tokio::time::timeout canceled its own future correctly but the actual hung
child process lived on forever, and something in its inherited-handle chain
(the exact mechanism wasn't pinned down further) kept the whole test/pipeline
from ever reaching EOF. One-line fix: cmd.kill_on_drop(true) in
ensure_connected's command config closure. Removed the temporary diagnostic
test module used to isolate this. Controller independently verified directly
(not delegated): cargo test -p gateway-registry (36 tests, all green,
timeout tests ~1s not hanging), cargo test --bin agentflare mcp_server::
(15 tests green), cargo build --workspace && cargo test --workspace (fully
green). Also read every changed file in the fix diff directly (config.rs,
error.rs, lib.rs, registry.rs, mcp_server.rs) — confirmed GatewayError::Timeout
falls into gateway_execute's existing internal_error catch-all with no
match-arm changes needed, confirmed the two new config.rs tests
(auth_ref-without-auth_env and vice versa) genuinely exercise the new
IncompleteAuthConfig rejection, confirmed both new eprintln! sites only log
identifying info (server name / auth_ref name) never secret values. Committed
as a8da060. A dispatched re-review subagent had independently confirmed
everything green just before being interrupted by a session reload; given the
controller's own direct verification already covered every claim in that
review's checklist, did not re-dispatch — proceeding straight to
finishing-a-development-branch.

ALL WORK COMPLETE: 13 tasks + 1 final-review fix, 16 commits total
(68cd5dd..a8da060), full workspace test suite green, real end-to-end smoke
test passed against the actual compiled binary.
