# agentflare rules

Static fallback for agents with no MCP support and no hook mechanism (e.g. Aider).
Everything else (Claude Code, Codex, Cursor, Windsurf, VS Code/Copilot, Cline,
Continue) gets a real integration via the `agentflare` CLI — see
https://github.com/getappz/agentflare. Use this file only if your tool isn't
one of those.

## Flare optimize module

agentflare ships a single consolidated compression/optimization module (`optimize`)
with four layers:

| Layer   | Command                       | What it does                          |
|---------|-------------------------------|---------------------------------------|
| output  | `agentflare optimize output`  | LLM-based prose compression (was caveman) |
| code    | `agentflare optimize code`    | Lazy senior dev code minimalism (was ponytail) |
| context | `agentflare optimize context` | On-demand BM25 relevance scoring over a session transcript (`score`); the `PreCompact` hook is inert |
| runtime | (automatic via hooks)         | Session hygiene, model routing nudges  |

`agentflare flare` / `agentflare opt` still work as backward-compatible aliases for
`agentflare optimize`. `caveman`/`ponytail` are not live command aliases — only
legacy `~/.config/{caveman,ponytail}/` cleanup on uninstall.

`agentflare optimize retrieve <id>` (and MCP `mcp__flare__optimize
action=retrieve`) recovers an original that the output layer compressed away
(CCR pattern). lean-ctx-compressed *reads* are instead recovered via
`ctx_read mode=raw` — agentflare does not re-cache them, because lean-ctx is
a separate sidecar not in agentflare's read path.

## Context compression — lean-ctx

**MANDATORY for code intelligence — do NOT use native Grep / Read-on-full-file /
shell `cat`/`grep`/`rg`/`find` to search or read code. Route ALL of it through
lean-ctx instead.** lean-ctx is in shadow mode: native file/search/shell calls
auto-route to `ctx_*` — but the rule below is the contract so agents without
shadow routing (Aider, plain shells) still comply.

- **Code search** → `ctx_search` (action=regex | semantic | symbol), NOT Grep/grep/rg.
  - exact symbol: `ctx_search(action=symbol, name=...)`
  - by meaning: `ctx_search(action=semantic, query=...)` (uses the on-demand
    dense index — no pre-build needed)
  - by pattern: `ctx_search(action=regex, pattern=...)`
- **Callers/callees** → `ctx_callgraph` (NOT grep for "who calls X").
- **Orient in unfamiliar code** → `ctx_compose` FIRST (one call vs
  search→read→search chain).
- **Read files** → `ctx_read` (compressed reader), prefer mode=anchored/full.
  Recover a compressed read verbatim via `ctx_read mode=raw`.
- **Shell** → `ctx_shell` (auto-compresses output).

Native `cat`/`grep`/`rg`/`find`/`Read`-whole-file are ONLY for: writing files,
git status/diff you will act on, and non-code text. Everything code-intelligence
goes through lean-ctx so the index stays the single source of truth.

```bash
npm install -g lean-ctx-bin && lean-ctx onboard
```

If `ctx_*` tools are genuinely unavailable in your runtime, fall back to the
native Grep/Read — but that is the exception, and you must say so.

## Cross-session memory

agentflare ships persistent memory in the binary itself — no separate
install. Recall relevant context at session start via the CLI (works even
without MCP support):

```bash
agentflare memory context
agentflare memory search "<query>"
```

Storing new memories (`memory_remember`) is exposed as an MCP tool; if your
tool has MCP support, prefer it there. Recall-only via the CLI otherwise.

## Web search

Use Exa for internet search when available — free-tier, no API key required.

## Browser automation

agentflare ships an agent-first browser automation CLI (thin frontend over a
Playwright-based sidecar, auto-installed on first use):

```bash
agentflare browser open <url>              # launch + navigate
agentflare browser snapshot                # accessibility tree with @e refs — primary page read
agentflare browser click <ref-or-selector>
agentflare browser fill <ref-or-selector> <text>
agentflare browser get text|html|value|title|url [target]
agentflare browser read [url]              # agent-readable markdown of the page
agentflare browser screenshot [path]
agentflare browser eval <js>
agentflare browser batch "<cmd1>" "<cmd2>" # multiple ops in one round-trip
agentflare browser close
agentflare browser doctor                  # diagnose the install
```

Sessions are isolated per working directory by default (`af-<hash>`), or set
explicitly with `--session <id>` / `$AGENTFLARE_BROWSER_SESSION`, so
concurrent worktrees don't collide. Run `agentflare browser --help` for the
full subcommand list (also: type/press/hover/select/check/uncheck/back/
forward/reload/tabs/cookies/storage/network/dialog/state/wait/pdf/extract).

## Rust builds — mbx shared cache (item #330, replaces #133/#139's sccache)

Claimed worktrees and dispatched agents build Rust through
[mbx](https://crates.io/crates/mbx) (`cargo install mbx --locked`). mbx keeps
compiled crates in one content-addressed store shared by every checkout and
replaces each checkout's `target/` with a symlink into its own managed
directory (`~/.cache/mbx/targets`), so a worktree whose store is warm builds in
seconds (`cargo build -p flare-git-core`, cold target: plain 310 s, sccache
147 s, mbx warm 48 s). Local workspace crates are still keyed by content, so
worktrees never see each other's stale artifacts.

- **No generated cargo config.** Worktrees get no `.cargo/config.toml` — no
  `rustc-wrapper = "sccache"` (mbx is the wrapper; two on one build is
  unsupported) and no `target-dir` (mbx owns `target`). Pre-#330 generated
  configs are removed on the next claim of that worktree.
- **Cache location.** `MBX_CACHE_DIR`, else `$XDG_CACHE_HOME/mbx`, else the
  platform default (`~/.cache/mbx` on Linux). The bwrap job sandbox binds that
  path read-write over its otherwise read-only `~/.cache` (created if missing)
  when it lies under `$HOME`; a path outside `$HOME` is not writable in the
  sandbox (`agentflare doctor` reports it).
- **Agent PATH.** `agentflare run` / `agents launch` prepend mbx's standalone
  cargo shim dir (`~/.local/share/mbx/bin`, installed by `mbx setup`) to the
  agent's `PATH`; the sandbox inherits it. No shim installed: use
  `mbx build|test|clippy` explicitly. They still strip an ambient
  `CARGO_TARGET_DIR`, which would bypass mbx's managed target.
- **Doctor.** `agentflare doctor` reports whether mbx is installed, its cache
  path, whether that path is writable inside the sandbox profile, and prints
  the install hint when missing.
- **Opt out / fallback.** Without `mbx` installed everything falls back to
  plain cargo (no sccache). To opt out with it installed, uninstall mbx or run
  `cargo` by absolute path (`~/.cargo/bin/cargo`).
- **Disk.** Defaults are kept (`gc.min_free_size` = 10 % of disk; mbx GC prunes
  its store and managed targets itself). Inspect with `mbx gc --dry-run`;
  `mbx settings set gc.min_free_size …` only if that is unworkable.
  `agentflare clean --artifacts` is the manual lever for other build dirs; it
  never offers a managed `target` symlink — use `mbx gc` / `mbx clean` for those.
- CI keeps its own sccache setup (no remote mbx cache yet).

## Node dependencies — shared store (item #329)

Claiming a worktree no longer means a second `npm ci`. `create_worktree_for`
(`flare-git-core::deps`) materialises each project's `node_modules`
(`package.json` + lockfile within two levels of the root) from
`~/.agentflare/deps/<lockfile-sha256>-<os>-<arch>/`, seeding that entry from
the main checkout's install when its lockfile is byte-identical. Files are
hard-linked from the store (zero extra bytes on ext4), falling back per file
to `std::fs::copy` (reflink on btrfs/XFS, and the cross-device case). The store
entry itself is seeded by a real copy of the main checkout's `node_modules`, so
a later in-place edit or postinstall in main never reaches it.

- **Share only on an identical lockfile.** Otherwise nothing is linked and the
  worktree log says `deps: <dir>: no shared install for this lockfile …`; run a
  normal install. Recognised: `package-lock.json`, `npm-shrinkwrap.json`,
  `pnpm-lock.yaml`, `yarn.lock`, `bun.lock(b)`. A store entry is built beside
  its final name and renamed in, so concurrent claims never see a partial tree.
- **Writes.** Hard links share inodes, so in-place writes would leak. Provisioned
  dirs carry a `node_modules/.agentflare-deps` marker and the bwrap sandbox
  overlays them (`--overlay-src`/`--tmp-overlay`): jobs read the shared bytes,
  their writes are private and discarded. `npm ci` recreates files and is safe.
  **Unsandboxed sessions get no overlay:** an in-place edit of a provisioned
  worktree's `node_modules` reaches the store entry and every worktree on it.
- **pnpm projects:** also set `virtualStoreType: global` in `pnpm-workspace.yaml`
  (`enableGlobalVirtualStore: true` before pnpm 11.23) so the store holds only
  symlinks. Not automated.
- **Clean.** `agentflare clean --artifacts` never offers a marked
  (hard-link-provisioned) or symlinked `node_modules`. It lists a store entry as
  a `deps store` item only if no checkout it can see references the lockfile
  *and* provisioning has not touched it for 14 days (the store is shared by all
  projects, so other projects' checkouts are invisible to the scan); half-built
  `<key>.tmp-<pid>` dirs from a crashed seeder are reclaimed after an hour.
- Cargo `target/` is out of scope (see mbx above).

## Git

Never add "Generated with Claude Code" or "Co-Authored-By: Claude" signatures.
Commit messages are the message only.

### SSH push / `gh` cache failures in a sandboxed dispatch (item #241)

A sandboxed job (`agentflare-jobs`'s bwrap wrapper, `--unshare-user`) can make
`git push` over an SSH remote (e.g. `git@github-appzdev:...`) fail with `Bad
owner or permissions on /etc/ssh/ssh_config.d/20-systemd-ssh-proxy.conf` —
root-owned files outside the sandbox's single mapped uid render with an
unexpected owner, which trips OpenSSH's strict check on `Include`d config
files. Workaround: push over HTTPS using `gh`'s stored credentials instead of
the SSH remote:

```bash
git -c credential.helper='!gh auth git-credential' push https://github.com/<org>/<repo>.git <branch>
```

The same read-only-root cause breaks `gh run view --log` / `gh pr checks`
(`read-only file system` writing to `~/.cache/gh`) — redirect the cache dir
first:

```bash
export XDG_CACHE_HOME=/tmp/gh-cache && mkdir -p "$XDG_CACHE_HOME"
```
