# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.8.0](https://github.com/getappz/agentflare/compare/v1.7.0...v1.8.0) - 2026-10-05

### Added

- add agentflare code impact (#397)
- add agentflare config CLI (set/get/unset machine-name) (#467)
- add agentflare git ship -- push+PR+CI-wait in one command (#391)
- add `apps run` command (Task 5) (#612)
- add Cline/ClinePass api.cline.bot provider support
- add Cline CLI support with ClinePass model routing (#564)
- add ClinePass multi-model serve script
- add dispatch preflight/init validation step (duplicate-work check + workflow-store smoke test) (#596)
- add encrypted SQLite and caller-owned transactions
- add /flare:resume MCP prompt + resume tool with explicit-id and confirm-gated latest modes
- add friendly machine-name storage to bridge config (#434)
- add pm MCP tool for reports and PM-mode toggle from any client (#676)
- add pm prompt so /flare:pm works in every project (#782)
- add pre-write YAGNI gate for Write/Edit (#390)
- add redispatch action for AI-safe re-arming of stuck items (#520)
- add review-only mode to sdd_loop (#547)
- add run_if support to JSON workflows + repo-compare workflow (#593)
- add SQL-level paginated item listing
- add supervisor auto-dispatch loop (item #408) (#376)
- add WorkflowStatus::Waiting for Sleep/SleepUntil/WaitEvent suspension (#567)
- adopt mbx as the shared Rust build cache for claimed worktrees (replaces sccache) (#849)
- agentflare clean — plan-then-confirm cleanup of merged branches, worktrees and build artifacts (#844)
- agentflare doctor checks Telegram chat-channel health (#679)
- agentflare-native cross-agent continuity phase 1 (#674)
- agent/model router - task-attribute routing, agentflare work integration, config.toml loader (#380)
- alert on sustained ScopeCheckError audit events (#493) (#529)
- async sqlx repository derive macro (CRUD codegen) (#777)
- authored split fields on handoff input (#674)
- auto-label brand-new items ready-for-work
- auto-merge and merge queues, draft PRs, ETag polling, pinned MCP merges (#809)
- autonomous in-review sweep — poll PR checks, self-repair on failure, promote on merge (#435)
- auto scale warning when >50k chunks + >100ms
- browser automation via agent-browser sidecar (#680)
- bundle SQLCipher with external OpenSSL SDK (#824)
- capacity governor / backoff for `work` autonomous job runner (#417)
- cap consecutive identical sdd_loop dispatch failures before auto-redispatching indefinitely (#557)
- capture stdout+stderr to a log file, add daemon logs (#424)
- category taxonomy + skill_categories tool (#618)
- claim marker prevents duplicate item creation across workstations (#632)
- Claude usage-threshold fallback to opencode (SDD implementer role) (#533)
- consolidate pm-mode into pm skill, embed in binary, persist PM mode per session (#538)
- daemon polls a registry of repos, not one env var (#422)
- deny agentflare work when invoked by an AI agent (#484)
- detect real merge conflicts in review sweep, optional auto-resolve dispatch (#774)
- drive review-bot threads to resolution (#813)
- durable SleepUntil step mode (#497)
- durable subagent-driven-development pipeline on flare-workflow (#498)
- embedded durable workflow engine for agent orchestration (#472)
- fail-closed command approval gate (item #197) (#768)
- failover router with exhaustion signals (#674)
- fall back to Cline CLI login when api key env unset
- fingerprint-based completion gate verification and review evidence (#313)
- GitHub as a coordination substrate for multiple agentflare instances (#379)
- GitHub bridge dogfooding — memory sync fix, handoff→queue publish, capacity signal, claim dispatch (#412)
- goal/quota precedence-ordered dispatch decision (#392)
- group job rows by item, tag self-repair jobs with why they fired (#489)
- harden multi-agent item processing; add job controls, agent failover and realtime agent messaging (#808)
- host resource gate throttles autonomous dispatch on CPU pressure (#459)
- idle-timeout instead of fixed wall-clock timeout for dispatched work
- Implement combined completion gate (verification-before-completion + finishing-a-development-branch) (#581)
- implement saga rollback/compensation handlers (#499)
- instance/step metrics query layer (workflow_metrics) (#509)
- Jev-backed decision layer (decide) with shadow-first router, skill rerank and SDD judge comparison (#851)
- local-first hybrid search — text-splitter chunking, fastembed vectors, RRF fusion, rerank, meta/path filters, similarity cache
- local query rewriting via fastembed (rule + SPLADE stub)
- Medusa-style DAL features (field policy, auto fields, errors, events, upsert, search) (#834)
- notify Telegram at every human-approval gate, not just three of six (#633)
- notify Telegram on human-in-loop supervisor gates (#615)
- on-demand sources adapters and send-to-inbox (#674)
- opencode-go usage-threshold fallback (5h/weekly/monthly) (#539)
- opt-in local training-data capture for distilling rules (#709) (#853)
- optional TDD mode for SDD workflow tasks (#592)
- plan-approval gate for item claim/dispatch (item #573) (#678)
- PM skill pack fast-follows — single /pm command, portfolio roll-up, real health bottlenecks (#474)
- PR & issue attribution — machine footer/labels (item #61, Tasks 3-5) (#518)
- project-level performance review (loopx Loop Engineering principle 7) (#404)
- project-local named workflows + per-step model/args/timeout overrides (#510)
- project-mode apply to instruction files (#674)
- registry-driven provider system with Anthropic/Gemini/Cloudflare AI Gateway support (#439)
- resource-aware work_max_concurrency (CPU + memory, ported from codegraph's resolver-pool) (#436)
- resume provider sessions across sdd-loop turns (#563)
- review sweep auto-tracks trusted-author PRs opened outside item done (#631)
- review sweep auto-updates a cleanly-behind PR branch via GitHub's update-branch API (#630)
- rewrite 4 curated ruflo skills as agentflare SKILL.md files (#442)
- rich Telegram card with Approve button for PR-approval gate (#675)
- sandbox job commands with bubblewrap on Linux/WSL2 (#420)
- SDD loop: commit progress after each implementer turn, squash at finalize (#622)
- select bundled or external SQLCipher at build time
- self-refresh expired Cline CLI tokens
- semantic embedding search over skills (#617)
- sqlite-vec vec0 ANN scale lane for >50k chunks
- step execution context + durable loop iteration resume (#502)
- store backfill/stats/rebuild — hybrid search maintenance
- surface flare doctor --reclaim as an MCP action (item action=doctor) (#487)
- surface instance/step metrics via CLI and MCP (#517)
- survive a bounded opencode.log tail past headless dispatch teardown
- sync observations across workstations via a shared git branch (#400)
- task branch names include a title slug (task/NN-short-title) (#451)
- task-type-based delegation routing heuristic (#427)
- tiered severity-routed escalation for critical/high vents (#429)
- typed item relations (blocks/duplicate/relates_to)
- typed params channel alongside the input string pipeline (#507)
- unified observability crate for AI agent sessions (#629)
- update_state accepts state_name or state_group, not just state_id (#461)
- warn on reset --soft/--mixed onto a diverged target (#462)

### Fixed

- abort rewind restore when the pre-restore snapshot fails (#602)
- actually exclude docs-only .md from build-matrix path filter (#511)
- address PR 840 review (per-day snippet needs 'limit'; drop ledger line on missing tier retry) (#841)
- address review findings on PR #815
- address round-3 review findings on PR #815
- address round-4 review clusters on PR #815
- adopt agentflare-db-kit migrations preventively (#573)
- adopt agentflare-db-kit migrations to fix missing-column schema drift (#572)
- agent-invoked calls can no longer self-clear the canonical-mutate guard (#803)
- align the outer job timeout with agentflare work's new hard cap
- allow checkout/switch to protected branch when working tree is clean (#485)
- always report consolidate outcome, not just non-empty filings (#386)
- apply filtered PATH to run_in_lines_bounded + fix orphan-reconcile label/assignee ordering (#556)
- attach a PR to the item that owns its branch instead of minting a duplicate item (#852)
- auto-commit uncommitted changes on item done, instruct agents to commit (#431)
- automated item done never populates reply_text, so every PR body is a placeholder (#590)
- bind ~/.agentflare read-write into job sandboxes (#500)
- bind claude-code's ~/.claude writable in bwrap sandbox (item #127) (#508)
- block item done from completing when push/PR creation fails on real commits (#482)
- bound caller-agent detection to the ancestor chain (Windows nextest hangs, item #314) (#811)
- bound the proxy readiness probe with a real connect timeout (#571)
- build_prompt never read handoff assets, so item_id-targeted handoffs never reached dispatch (#448)
- bwrap-sandbox the headless coding-agent subprocess (#445)
- canonicalize both sides of the workspace-membership check (#440)
- cap claim TTL to the short default once an item reaches in_review (#481)
- cap comment-thread size injected into dispatch prompts (#450)
- cap success-comment reply size, offload large output to an asset (#444)
- capture invoking PATH into generated systemd unit / launchd plist (#494)
- carry owner identity into finalize step across engine threads (#531)
- centralize console-safe spawning in flare-process to stop terminal flashing
- check CodeRabbit findings before merging an approved PR (#793)
- check daemon restart, deadline readiness poll, pass proxy token to claude
- chunks_exact_to_as_chunks + result_large_err from a newer stable clippy (#574)
- claimed item's worktree gets wiped (all tracked files deleted) across job death / daemon restart / redispatch, and sdd_loop then commits the wipe (#832)
- claim ownership match must be instance-scoped, not agent-type-scoped (#458)
- clean up the worktree on a plain release, not just done/check_merge (#388)
- clear CodeQL cleartext-logging on PR 815 test asserts
- clear dispatch labels on cancel, make discovery state-aware (#661)
- close 3 review gaps left by item 595's PR identity tag (#790)
- close dispatch-failure-cap gap that let orphan-restarted items retry forever
- close path-traversal and hard-coded-crypto CodeQL alerts (#781)
- crash-resume doesn't actually resume workflow_run_id after daemon restart (#578)
- crash-resumed work-item runs must not rely on ambient cwd for agent dispatch (#619)
- cursor-agent headless dispatch hang on Windows (#544)
- Daemon restart orphan-recovery: verify running binary has the fix, jobs failing with no captured logs (#609)
- deadlock in doc_upsert new-doc path — drop conn before sync_chunks
- defer CI self-repair instead of dispatching into a claim it can't win (#488)
- defer repair when another worktree holds the PR branch; notify on a gated green PR (#856)
- delete_state deletes workflow_runs by wrong column (#576)
- derive Clone on gateway_registry::ServerConfig (#610)
- derive conventional-commit PR titles instead of using raw item name (#568)
- descendant_pids Windows stub was unreachable dead code, test-only import unused off Linux (#460)
- detect and surface expired agent auth instead of silent retries (#666)
- detect a stale binary and self-restart instead of running outdated in-process job logic (#466)
- detect_review_only doesn't classify design-spec tasks as no-code (#566)
- detect_review_only false-positives on any description merely mentioning a finished design-spec (#591)
- differentiate design-spec mode_note from plain review-only (#663)
- disambiguate the claim tool from item(action=claim) in every denial message (#385)
- discovery/dispatch/execution resolve each item's own project dir, not the daemon's cwd (#437)
- dispatch cursor-agent with stream-json to avoid idle-timeout false positives (#183) (#594)
- dispatched jobs inherit the wrong timeout (300s, not work's own 1800s) (#395)
- dispatch onto an item's tracked PR branch instead of a fresh task/<seq> branch (#673)
- distinguish a failed commit from nothing-to-commit in item_done (#455)
- distinguish scope-check classification failure from a real policy Deny (#532)
- document every subcommand's --help and unify status output (#522)
- don't treat a self-repair job's own open PR as a duplicate
- drop unrelated cosmetic diff on opencode-branch-guard (indent + em-dash)
- enable loginctl linger so Linux autostart survives headless reboot (#476)
- enforce go/no-go pending-decision gate with a label, not prose
- fail structural worktree-setup failures straight to terminal (#486)
- fall back to a fresh session inside the same attempt when --resume is stale (#850)
- Follow-up to #695: hook deadline budgets from configured timeouts, explicit --agent claude-code, doc-comment fixes, behavioural tests (#848)
- gate classify() denies on agent-invocation, not just tracked-repo scope (#389)
- gate the github bridge on author_association, frame issue content as untrusted (#418)
- guarantee a claim release on every execute_work exit path (#470)
- handoff clears stale dispatched/needs-manual-dispatch labels like redispatch does (#627)
- handoff sets metadata.task_type so detect_review_only stops scanning free text (#624)
- hard delete must clear chunks before documents (FK)
- harden kill_tree against taskkill's tree-kill race, add leak detection (#443)
- harden parse_judge_decision against malformed judge replies (#516)
- harden tree fingerprint for completion gate (CodeRabbit #830)
- headless dispatch prompt must forbid backgrounding verification (no resume mechanism exists) (#438)
- headless work-item dispatch flashes a visible console window on Windows (#473)
- honor Last-Event-ID on job log stream reconnects (#377)
- honor RetryPolicy for StepMode::Loop iterations (#519)
- item done no longer auto-pushes with no opt-out, and cleans up its own worktree (#382)
- item_id re-labeling permanently blocked by stale-but-undone claims (#446)
- item metadata double-encoding + stale daemon dispatch binary (#403)
- jobs list badge distinguishes success from failure (#430)
- keep encryption keys out of diagnostic options
- keyring-only DEK session cache, delete weak XOR file fallback (#799)
- kill_graceful reaches descendants that escaped into their own process group (#394)
- let [router] model rules reach daemon-dispatched items (#577)
- log the reason for every pre-pipeline dispatch failure
- log when a dispatched item queues behind the worktree-cwd lock (#635)
- make binary-staleness watchdog failures observable, add task-level test (#480)
- make rule-file test cleanup best-effort instead of unwrap()
- make test temp dirs unique across processes, not just in-process (#405)
- make WorkItemData self-sufficient for crash-resume (#561)
- make worktree snapshot temp index unique per call, not just per process
- make worktree-teardown deny message and doctor reclaim actually work (#493)
- map opencode's --auto flag so autonomous dispatch accepts it (#468)
- merged duplicate PR no longer auto-completes an item; PRs carry item UUID (#595) (#789)
- mount ~/.config/cursor for cursor-agent headless jobs (#543)
- mount ~/.cursor into the bwrap sandbox for cursor-agent jobs (#540)
- neutralize process-tree agent detection for human_shim tests (#559)
- never rebase an already-pushed item branch; follow origin instead (#847)
- no_window unused-param on unix; start /B for deferred swap; assert captured stdout
- only unwrap success envelope when success is true
- OpenAI-compatible tool_choice shape + duplicate message_stop (#560)
- orphan-reconcile clears assignee_agent, blocking auto-redispatch after daemon restart (#551)
- orphan reconciliation never killed a still-alive subprocess, letting two agents race the same worktree (#582)
- orphan restart release must not delete clean worktrees (#839)
- orphan/terminal-failure reconcile restores dead job's frozen agent instead of the item's current assignee (#670)
- overlay opencode's data dir so headless dispatch survives the read-only bwrap root (#479)
- ownership escape hatch, crash-vs-deny clarity, -C read-only allowlist (#527)
- paginate auto-pick past first page, hermetic confirmed-send test with RAII env guards
- parse Claude Code stream-json reply before threading it downstream (#546)
- pipe headless prompt via stdin instead of argv (#441)
- plan-approve channel tap resolves the item in its own project (#784)
- Port superpowers' systematic-debugging skill as root-cause-first methodology (#587)
- PostToolUse matcher omitted ReportFindings, so review evidence was never recorded (#610) (#788)
- PR-approval Telegram ping still fires early despite "blocked" guard (item #587) (#770)
- preserve plan approval fields through unrelated metadata updates (#802)
- preserve sandbox for autonomous dispatch
- PR gets two different beacon:flared:<machine> labels from two workstations working the same item (#694)
- PR opened by item done uses the agent's own summary, not a placeholder (#419)
- pushed_branch() mishandles src:dest push refspecs (#387)
- raise compare step idle timeout to 900s to survive slow ctx_search sweeps
- raise idle-timeout default from effectively-disabled to 30 minutes (#671)
- rebase onto latest default branch before dispatch and before push (#583)
- recognize PR creation failed and commit failed comments as terminal dead-claim evidence (#819)
- reconcile agent_jobs rows orphaned by a daemon restart on startup (#453)
- recover schema-ahead DB migration + worktree branch-resolution collision (#471)
- redispatch to another agent stops the previous agent's job (#607) (#787)
- register flare MCP server at Cline CLI's actual config path (#565)
- reinstate anti-backgrounding instruction in build_implementer_prompt (item #71 regression) (#657)
- reject plan_required with no assignee_agent and no plan (#796)
- release/done no longer silently no-op on claim identity mismatch (#447)
- release item claim on finalize success (#555)
- release must not erase a reassignment made mid-run (#783)
- release the orphaned job's claim inside restore_ready_for_work too
- remove unenforced playwright-mcp docstring claims (#682)
- rename flare_git to git; add pm MCP tool (#542)
- renumber item dates migration 0012 -> 0013
- replace removed --full-auto with --dangerously-bypass-approvals-and-sandbox
- re-resolve assignee + model from live item at execution time (#659)
- researcher example uses error_mode=skip on fan-out sweeps (#526)
- resolve git worktree .git indirection for bwrap commits (#454)
- resolve project_dirs/bridge_repos through worktree_repo_root() (#463)
- resolve sequence_ids in comment and honour parent_id on item update (#496)
- resource dispatch gate has no reset mechanism, stays paused indefinitely (#805)
- restore in_review state after a failed repair claim (#818)
- restore process-tree feature dropped by hakari workspace-hack (#410)
- restore ready-for-work label when reconciling crash-orphaned jobs (#465)
- restore recipient invariant, bound 429 signal match, dedup files O(n) (#817)
- retry lock acquisition on any error, not just AlreadyExists (#433)
- retry the pre-branch git fetch once and log the real failure reason (#528)
- retry transient registration races, guard dirty checkout, back off redispatch churn (#798)
- reuse an existing worktree found on disk, not just via cwd (#414)
- Review sweep: detect CodeRabbit findings, dispatch a fix, post a summary of what was addressed (#760)
- review sweep now posts GitHub-visible stage labels/comments on the PR (#672)
- review-sweep self-repair dispatch ignores multi-project support (single-project scan + no folder_path pinned) (#505)
- run saga rollback on explicit cancellation (#501)
- scope auto-pick to the current project and exclude the calling session by id (#859)
- scope-check subprocess crash denies legit push (issue #483 bug #2) (#513)
- scope doctor force-reclaim to a single worktree (#515)
- scope item id resolution to the caller's project
- scope item relation endpoints to the caller's project
- SDD judge JSON and usage-limit cooldown alignment (#840)
- sdd_loop discards the real failure reason at 6 call sites (bare StepResult::Failure, no error text) (#585)
- SDD-loop judge-parse retries + item_done in_review no-op (#512)
- second review round on PR #815
- seed the instance id from a machine id (#418) (#381)
- self-heal items that regress out of in_review with a tracked open PR (#234) (#691)
- self-repair falls back to [router] rules when an item has no assignee (#674)
- serialize concurrent git worktree add calls (#595)
- serialize execute_work's worktree chdir against concurrent dispatch (#601)
- serialize Telegram card/notification sends against in-flight chat turns, surface a session reset (item #281) (#775)
- set sdd_loop step timeout to match WORK_JOB_TIMEOUT_SECS (#521)
- shortcircuit's extract_filepaths treats shell redirects as literal filenames (#396)
- single-flight guard in dispatch_item (item #221) (#660)
- skip foreign-typed rows in list_active/list_all instead of failing the whole scan (#514)
- speed up sync and dashboard, add sync progress
- stop CodeRabbit cap and CI-green supervisor flip-flop (#833)
- stop force_resume/sampler from blocking current_policy() reads (#806)
- stop killing headless SDD dispatches at a 300s idle-timeout (#549)
- stop one item's dispatch retry from destroying another item's uncommitted work
- stop the two process storms (dispatcher re-entry fork-bomb, cross-project dispatch retry loop) (#759)
- supervisor-dispatched jobs can claim their own assigned item (#393)
- suppress console flash in run_real's real-binary exec (#477)
- surface a real diagnostic when claim's worktree resolution returns None (#586)
- surface captured output on a headless agent timeout instead of discarding it (#398)
- surface why a claimed item never gets worked (#423)
- swap dispatched label off items whose job cleanly failed all retries (#463) (#478)
- telegram poller offset persistence bugs (CodeRabbit findings on #678) (#763)
- three dispatch-reliability bugs found dogfooding autonomous work (#413)
- tighten repo-compare check-cache reply + skip permissions on record step
- tolerate omitted completed/remaining in MCP deserialization (#548)
- unwrap flare-gateway calls before completion-gate classification (#653)
- use cheap KDF params in test builds, not production Argon2 cost (#814)
- use dunce::canonicalize for repo-root paths instead of std::fs::canonicalize (#475)
- use MCP-safe swap for shim binary placement (#457)
- use stream-json output so idle-timeout gets real liveness signal (#415)
- use tempfile::tempdir() instead of a process-local counter
- validate /pm prompt command argument instead of interpolating raw input (#786)
- validate recipient against agent registry, deny with suggestion (#428)
- vault error message points at 'vault unlock' instead of a non-existent interactive prompt (#685)
- verify a matched PR is this item's own before trusting it (item #63) (#608)
- verify the installed binary after a swap, not just dev-install (item 627) (#795)
- verify the installed binary and stop a locked .old.exe forcing a silent deferred swap (item 624) (#791)
- Windows console-flash — CREATE_NO_WINDOW on daemon/gateway/git-shim/jobs spawns (#407)
- windows git shim staging always deferred, never installed (#575)
- wire filtered PATH through all git spawn sites
- worktree residual polish — teardown messaging + gh merge collisions (#456)
- worktree stale-registration bug + item-pipeline metadata panic (#550)

### Changed

- Add one-call work-item status aggregator (item + workflow + PR + daemon-log) (#801)
- add pm-mode — delegate/validate/dispatch/report (#426)
- Audit and wire download/install flows through the existing with_spinner UI helper (#764)
- Auto-dispatch dependents when a blocking item completes (dependency-graph-driven dynamic workflows) (#621)
- Borrow (Notis): pre-handoff secret scan (#669)
- cap nextest slow-timeout at 60s, fix sccache stats on windows (#812)
- clinepass-proxy.ps1 starts daemon proxy then launches claude
- comprehensive refresh of README, docs-site, and marketing site (#373)
- consolidate ~/.agentflare path construction into one function (#769)
- consolidate secret CRUD into agentflare vault (#378)
- Cursor hooks: expand wire_cursor to Claude/Codex depth (#838)
- Cursor insights: scrub advertised support (no adapter) (#836)
- Cursor messaging: enable context inject (#845)
- design-spec for saga rollback/compensation handlers (#495)
- design-spec for StepMode::Loop per-iteration journaling (#491)
- design-spec for systematic-debugging enforcement gate
- design-spec for typed/structured trigger params (#125) (#506)
- Dispatched agent's own subprocess never inherits its job's claim-owner identity (AGENTFLARE_SESSION not propagated) (#683)
- dispatch work items in-process instead of re-invoking `agentflare work`
- document creation-flags overwrite contract
- document SSH push / gh cache sandbox workaround (item #241) (#692)
- drop stale classify.rs and work_item_pipeline.rs allowlist entries (#580)
- Duplicate-work preflight check false-positives on a PR that only mentions the item number (#603)
- exclude doc-only files from the build-matrix path filter (#492)
- Extend the completion gate to require fresh code-review evidence, not just verification (#654)
- Extract agentflare-core foundation crate (paths/state/store/errors/dispatch_failure_ceiling/mise_install) (#816)
- extract bwrap sandboxing into a reusable flare-sandbox crate (#541)
- flare-workflow: command-step executor (no-LLM steps for mechanical JSON workflow actions) (#656)
- Gated force-override for item claim/release/done when a claim is live-but-stale on a confirmed-dead job (#807)
- Hooks regularly hit timeouts: agentflare PreToolUse/UserPromptSubmit/SessionStart latency (#843)
- ignore local .cursor/ config (#545)
- in_review sweep/list query misses an item with a valid state_id — stale read-index, not corrupted state_id (#810)
- item::claim() doesn't check item state — a stale queued job can silently reopen a cancelled item and re-run it (#662)
- item tool: structural filters + one-call annotations (unassigned/blocked/has_comments/stale_claim/unestimated) (#797)
- land metrics observability design spec with its implementation (#530)
- Make sandbox WRITABLE_HOME_DIRS runtime-configurable via env var (stop requiring a rebuild per dir) (#686)
- P0: team addressing, markers, busy flag, history replay, team launch (inter-agent chat) (#829)
- pace UserPromptSubmit PM mode and setup nudges (#842)
- pin cargo-hakari to 0.9.38 (unpinned latest 0.9.39 fails hakari on every PR) (#779)
- pin rust-toolchain to 1.98.0 to match CI's stable (#589)
- Race condition: agents can edit the shared main worktree instead of an isolated claimed one (#828)
- release v0.1.0 (#374)
- remove dogfood test file from item #421 verification (#384)
- Repair dispatch adopts the item's unfinished run: turn launches under a dead claim owner and with the original task prompt (#846)
- Resolve agent-browser via `mise env --json` instead of PATH scanning / mise where (#755)
- resolve agent-browser via mise which --tool instead of mise where (#753)
- run tests with nextest, add cargo-hakari workspace-hack (#399)
- rustfmt snapshot.rs after filtered PATH wiring
- Session checkpoint linkage + rewind/explain CLI (gap vs firecrawl/entire) (#600)
- skill_create scaffolding tool (go/no-go) (#613)
- submit_plan never sets plan_status, permanently blocking approve_plan (#800)
- Task 1: App manifest parsing crate (agentflare-apps) (#604)
- Task 4: app_send_hook — App-aware SendMessage (#611)
- Task 6 (spike): Ephemeral per-run MCP tool wiring (#605)
- Task 7: Port Auto-Company into the App format (reference App) (#616)
- Telegram chat channel for agentflare management (#677)
- Toolchain-aware skill_recommend tool (go/no-go) (#597)
- Vent escalation sweep has no exit condition once an item is triaged-but-not-closed — re-escalates forever (#761)
- Workflow-level failure discards the real per-step error, so rate-limit cooldown never fires for autonomous dispatch (#664)

## [1.7.0](https://github.com/getappz/agentflare/compare/v1.6.0...v1.7.0) - 2026-07-29

### Added

- `flare-vault` crate + `agentflare vault` CLI — local secrets vault with Argon2id/AES-256-GCM envelope encryption, OS-keyring session cache, and global/project scoping; replaces the sqlite-backed `gateway_secrets` store used by `agentflare gateway secret`, channel bot tokens, and GitHub auth (#370)
- agentflare-jobs crate — background job queue + process supervisor for agent CLIs (#298)
- auto-enforce core-module usage via init/SessionStart (#352)
- auto-release claim on cross-agent reassignment (#296)
- bound artifact version history and audit log growth (#289)
- bound the docs cache with retention, eviction, and page reclaim (#348)
- build and install PATH shims alongside the main binary (#301)
- CI paths in trust-root baseline + /flare:git slash command (#367)
- close 3 ponytail parity gaps (#310)
- core-module auto-enforcement, docs-site, flare-docs Python/npm examples, coaching MANDATORY tier (#361)
- cover the shell layer in agentflare init (bashenv guard, opencode branch-guard, config split) (#324)
- deny pushing the default branch (PR-only enforcement) (#312)
- finish EPIC #272 — Task 6 snooze/dismiss write-path + Task 9 provisioning (#354)
- flare doctor — claim-worktree health sweep + safe reclaim (#235) (#305)
- index per-item rustdoc docs, not just the crate overview (#323)
- intent-to-skill injection + skill install CLI (#231, #233) (#290)
- manage a flare-docs usage rule + add `agentflare doctor` (#325)
- npm/TypeScript ecosystem support; rename tool to `docs` (#342)
- on-demand third-party docs — flare-docs crate + flare_docs MCP tool + agentflare docs CLI (#316)
- only block trust-root pushes to the default branch (#317)
- path-scope enforcement for claims (QuorumGit adoption) (#303)
- Python ecosystem, README usage examples, and the docs site
- regex pre-filter for ponytail over-engineering checks (#293)
- repo-local verify gate + `pr_wait` action — bounded server-side poll loop for PR checks (default 60s, capped 120s per call), replacing manual `gh pr checks` polling loops (#118, #362)
- request-optimization short-circuits for CLI bookkeeping calls (#355)
- restyle statusline badge to Claude Code native hint style (#311)
- retire OpenResearch, fold 17-source search into flare_search (#326)
- share sccache across worktrees when available (#133) (#299)
- shared nudge-pacing primitive for coaching rules + PostToolUseFailure hook (#364)
- skill routing/management epic — FTS5, negation, bandit ranking, pack/hub lifecycle (#302)
- sweep to reclaim orphaned legacy shared blobs (#353)
- unified config.toml git-shim policy slice (#331) (#336)
- vent: origin routing, throttle, batched filing, judge-prompt hook (#331)
- verified continuation commit, structured payload, duplicate-item reuse, assignee freeze (#365)
- warn when editing on a branch stale vs origin/default (#360)
- worktree orphan audit + fix git_binary shims-dir self-deny (#304)

### Fixed

- caller-vs-service error mapping, search limit cap, non-blocking-fetch test (#344)
- case-insensitive + SQL-style SPDX in pre_filter noise patterns (#307)
- clear genuine ACL denials on orphaned worktree cleanup (#322)
- close branch-guard bypasses for bare filenames, new dirs, missing path fields (#291)
- close staging symlink/TOCTOU bypass, log swallowed blob-reclaim errors (#357)
- dedup documents before creating the unique index (#346)
- default-limit and paginate item(list) (#358)
- don't open a redundant PR when item done runs after the PR already merged (#329)
- flare-docs stale-item reconciliation + doc history-skip check (#337)
- give each db its own blob dir so GC can't delete a neighbour's content (#351)
- guard agentflare db files from agent deletion + fix artifact store history tracking (#335)
- log DB errors, validate cost by=, gate non-local bind, add HTTP tests (#369)
- never let doctor reclaim delete the main worktree (#327)
- release blob refs when documents are deleted (#343)
- reset recursion depth before exec'ing real git (#359)
- resilient worktree removal on Windows file-lock (#302) (#308)
- reuse an existing branch that owns no worktree instead of failing (#321)
- rivalsearch param name + store FTS query sanitization (#288)
- scope all classify.rs denials to agentflare-tracked projects (#320)
- stop `list` returning every cached document in full (#345)
- stop usetsearch blocking every native ToolSearch call (#368)
- surface worktree creation failure reason in claim response (#318)
- use plain tag ref for SLSA provenance reusable workflow (#314, #315)
- v1.6.0 verification bugs — opencode filePath guard hole, search web/store arms (#287)
- validate action before repo/client setup; resolve default branch via API (#363)
- vent tag union + flare-docs fetch-outcome observability (#338)

### Changed

- apply ponytail-audit findings (9 unused deps + 11 dead functions) (#330)
- cut the Windows build's redundant pass, Defender scanning, and PDB cost (#349)
- decouple agent-registry dependency (#297)
- drive every FTS5 index from triggers, not hand-written sync (#347)
- drop the Windows Defender exclusion step, it bought nothing (#350)
- flare-git-core: name touched trust-root paths in push deny message (#313)
- rename ponytail_engineering_check_internal to over_engineering_check_internal (#300)
- scoop manifest automation for release publishing (#286)
- split skill tool handler into its own module (#319)
- tune .coderabbit.yaml to reduce review rate-limit hits (#309)

## [1.6.0](https://github.com/getappz/agentflare/compare/v1.5.0...v1.6.0) - 2026-07-21

### Added

- global search MCP tool — store, memory, code, and web arms, with gateway delegation and local MCP auto-registration (#284)
- migrate asset store to agentflare-store documents+blobs (#282)
- git-aware PATH shim — classify, snapshot, canonical-repo detach guard (#279)
- cover opencode's native tool names in the branch guard (#281)
- release-bump.sh + release-tag.sh — manual release flow, no crates.io (#275)

### Fixed

- resolve PreToolUse branch guard against target file's repo, not host cwd (#283)
- sync session hooks with actual feature state, cut per-turn bloat (#277)
- don't run git worktree prune under --dry-run (#272)

## [1.5.0](https://github.com/getappz/agentflare/compare/v1.4.0...v1.5.0) - 2026-07-20

### Added

- `agentflare daemon {start,stop,restart,status,enable,disable}`: background daemon lifecycle (PID file, flock-based start lock, Unix socket / Windows named pipe IPC), autostart registration via launchd (macOS) / systemd --user (Linux), and a 24h-cached update check. HTTP-over-IPC client and macOS ad-hoc codesign scaffolding land ahead of the follow-up that wires the daemon's own HTTP handler and MCP tool-call dispatch.
- `flare-proxy` crate: Anthropic-to-OpenAI free-provider proxy, with env-var model routing (`MODEL`/`MODEL_OPUS`/`MODEL_SONNET`/`MODEL_HAIKU`).
- `@mention` feature: inline `@I`/`@A`/`@search` references resolved across items, agents, and search.
- `agentflare work`: autonomous claim → worktree → headless agent → report-back command.
- `vent` MCP tool + `agentflare vent` CLI: agents log tooling friction to an append-only per-repo JSONL; a deterministic classifier consolidates them once per turn (via the PromptSubmit hook) and auto-files actionable vents as backlog items. No new dependencies; fully auditable (raw `vents.jsonl` + `vent list`).
- Read-only dashboard (`agentflare serve`), Phases 0-2.
- `flare_git`: paginated list actions (no more silent 30-item truncation) and a `pr_status` action bundling PR detail + CI checks + reviews + comments into one call.
- `flare_handoff`: knowledge fact import + session snapshot on handoff.
- `agentflare-store` crate: initial KV/document (CRUD+FTS+vector+hybrid)/blob/lease engine.
- CLI branding banner (logo asset, installers, `about` command).

### Changed

- *(memory)* brain.db now opens through the shared db-kit engine (versioned migrations, WAL, FK enforcement); recall gains optional hybrid semantic search (BM25+vector merge, 30-day temporal decay, MMR) behind `--features semantic`, with `agentflare memory backfill-embeddings` to index existing observations. FTS-only behavior is byte-identical without an embedding model.
- `item`/`claim` tools accept a numeric `sequence_id` directly; new item IDs switch to nanoid.
- CI: LOC-gate wired into the pre-commit hook (staged files only).

## [1.4.0](https://github.com/getappz/agentflare/compare/v1.3.1...v1.4.0) - 2026-07-17

### Added

- *(github)* `flare_git` GitHub module — PR/issue/release/workflow-run models, auth token resolution, action-dispatch MCP tool, init-auth credential classifier ([#221](https://github.com/getappz/agentflare/pull/221))
- *(ui)* adopt cliclack for interactive CLI prompts and status output ([#220](https://github.com/getappz/agentflare/pull/220))
- *(optimize)* reversible-compression retrieve registry + `optimize retrieve` CLI/MCP actions (CCR)
- *(pm)* PM skill pack v1 — /pm:standup /pm:groom /pm:plan /pm:health
- *(coaching)* contextual coaching triggers (BM25 auto-match) ([#213](https://github.com/getappz/agentflare/pull/213))
- *(labels)* MCP list/update/delete + project-scope enforcement on attach ([#205](https://github.com/getappz/agentflare/pull/205))
- MCP-safe self-upgrade primitive + `agentflare dev-install` ([#206](https://github.com/getappz/agentflare/pull/206))
- *(maintainability)* adopt LOC gate from lean-ctx ([#218](https://github.com/getappz/agentflare/pull/218))
- server-side groom/standup/health/plan actions

### Fixed

- *(gateway)* init idempotency check survives malformed sibling entries ([#219](https://github.com/getappz/agentflare/pull/219))
- *(github)* validate workflow_dispatch inputs, percent-encode query values, detect stored github_token, retry RateLimited instead of erroring
- *(init)* guarantee a GitHub credential for github repos; skip PAT prompt under -y / non-TTY stdin
- *(mcp)* return text asset content as UTF-8, not base64; server-derive artifact sender ([#211](https://github.com/getappz/agentflare/pull/211), [#207](https://github.com/getappz/agentflare/pull/207))
- *(worktree)* skip PR when branch content already merged (squash detection); close ambient CARGO_TARGET_DIR gap for agent builds and CI ([#210](https://github.com/getappz/agentflare/pull/210), [#217](https://github.com/getappz/agentflare/pull/217))
- *(optimize)* persist retrieve originals in blob store; atomic+locked index; TTL-on-list; resolve clippy -D warnings; address CodeRabbit review (pct panic, list path leak, legacy CCR)

### Other

- site: agentflare.dev landing page + Cloudflare Workers deploy, self-hosted display font, OG social image
- refactor: consolidate compression into the `optimize` module; fold runtime submodule in
- hook: deprecate inert PreCompact FTS5 scorer, keep as no-op stub
- chore: fix pre-existing rustfmt drift; add opencode.json rust-analyzer config; exclude machine-local opencode.json and docs/

## [1.3.0](https://github.com/getappz/agentflare/compare/agentflare-v1.2.0...agentflare-v1.3.0) - 2026-07-12

### Added

- *(hooks)* dynamic memory nudge, agentflare: prefix, auto-detect agent
- *(agents)* headless agent invocation — run a prompt, capture the reply ([#151](https://github.com/getappz/agentflare/pull/151))
- *(init)* detect GitHub repos and register github-mcp-server behind the gateway

### Fixed

- *(mcp)* register memory tools with the tool_router so they're reachable
- *(headless)* use kill -s KILL -- <pid> to avoid CLI arg-parsing ambiguity
- *(run)* reject --print combined with --model/--mode/--env/trailing args instead of silently ignoring them — the headless path never threaded those through, so users had no signal their flags were dropped.
- *(headless)* kill the whole process tree on timeout, not just the direct child — a descendant holding the stdout pipe open (e.g. a grandchild spawned by claude -p / codex exec) could hang the reader thread forever, defeating the timeout entirely.
- *(init)* only print gateway follow-up note when registration succeeded
- *(init)* make gateway register() self-idempotent, not just caller-guarded

### Other

- add clippy, fmt, and cargo-deny gates behind a CI Green aggregator ([#158](https://github.com/getappz/agentflare/pull/158))
- address CodeRabbit findings on the engram-removal commit
- remove engram integration — replaced by built-in memory module
- Merge remote-tracking branch 'origin/master' into refactor/db-consolidate-secrets
- Merge remote-tracking branch 'origin/master' into feat/review-consensus
- cap build/test job at 25 min so a hung test fails fast instead of pinning a runner for 6h
- Merge remote-tracking branch 'origin/master' into feat/claim-ledger

## [1.2.0](https://github.com/getappz/agentflare/compare/agentflare-v1.1.0...agentflare-v1.2.0) - 2026-07-08

### Added

- skill registry MCP — skill_search + skill_load ([#92](https://github.com/getappz/agentflare/pull/92))
- *(ponytail)* per-session mode + status report ([#87](https://github.com/getappz/agentflare/pull/87))
- *(ponytail)* SubagentStart agent_type regex matcher ([#91](https://github.com/getappz/agentflare/pull/91))
- detect competing compression plugins during init ([#86](https://github.com/getappz/agentflare/pull/86))
- agent-detector process-tree detection + auto-wire ponytail hooks
- ponytail L1 integration — port runtime to Rust
- add apt PPA and Docker distribution channels
- add --reload-daemon and shallow profile isolation (#35, #36) ([#41](https://github.com/getappz/agentflare/pull/41))
- add eyre + color-eyre for rich error reporting ([#40](https://github.com/getappz/agentflare/pull/40))
- add thiserror typed errors (partial - auth, auth_runner) ([#39](https://github.com/getappz/agentflare/pull/39))
- adopt mise conventions - build info, edition 2024, lints, tooling ([#38](https://github.com/getappz/agentflare/pull/38))
- auth vault phases 3+4 - failover, isolation, encryption ([#23](https://github.com/getappz/agentflare/pull/23))
- auth vault phase 2 - rotation, cooldown, health scoring ([#23](https://github.com/getappz/agentflare/pull/23))
- add auth_db SQLite layer for health, cooldown, rotation state
- add auth profile vault (Phase 1, addresses #23)

### Fixed

- close ponytail parity gaps from upstream PR audit ([#61](https://github.com/getappz/agentflare/pull/61)) ([#96](https://github.com/getappz/agentflare/pull/96))
- post-1.0.0 code review — ponytail custom skills, auth health scoring, CI defects ([#94](https://github.com/getappz/agentflare/pull/94))
- remove hard-coded cryptographic salt (LEGACY_SALT)
- add SAFETY docs to unsafe set_var/remove_var blocks
- *(hook)* stdin timeout + stderr logging + bare /agentflare report ([#80](https://github.com/getappz/agentflare/pull/80))
- resolve all zizmor errors on master
- add .gitmodules for winget-pkgs submodule reference
- remove no-stale-brand job - pre-existing submodule corruption causes checkout cleanup failure (winget-pkgs/ phantom reference)
- correct sccache-action SHA
- pin all actions to commit SHAs, tighten release permissions
- CI - zizmor PR-only, security-check path filter, concurrency guard
- review fixes - encryption, format marker, Windows env, retry backoff
- add actions:write permission for nested workflow dispatch

### Other

- allow manual dispatch of release-plz workflow ([#97](https://github.com/getappz/agentflare/pull/97))
- *(cla)* skip the job entirely for maintainer and bot PRs ([#95](https://github.com/getappz/agentflare/pull/95))
- add .gitattributes for cross-platform CRLF handling ([#82](https://github.com/getappz/agentflare/pull/82))
- multi-crate workspace + mise-style CLI
- Revert "fix: remove accidental winget-pkgs submodule - manifests are in winget/"
- add winget auto-update workflow using komac
- add winget manifests for v1.1.0
- auth vault phase 2 implementation plan
- auth vault phase 2 design spec
- scoop manifest: agentflare 1.1.0

## [1.1.0](https://github.com/getappz/agentflare/compare/v1.0.2...v1.1.0) - 2026-07-06

### Added

- add agentflare alias command (closes #25)

### Other

- disable git release in release-plz (handled by release.yml)
