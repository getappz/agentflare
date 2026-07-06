# Feature: Auth Profile Vault & Account Switching (CAAM-style)

**Status:** draft
**Priority:** high
**Effort:** L (large — ~2 weeks)
**Depends on:** [#22 — agents list/doctor](https://github.com/getappz/agentflare/issues/22)

## Problem

Developers with fixed-cost AI coding subscriptions (Claude Max $200/mo,
GPT Pro $200/mo, Gemini Ultra $275/mo) hit usage limits mid-session.
The official account switch flow requires browser OAuth dance (30-60s).
Multiply by 5+ switches/day across multiple tools.

Each AI CLI stores OAuth tokens as plain files on disk. These can be
backed up and restored sub-100ms — no browser, no OAuth dance.

## Solution

Add `agentflare agents auth <subcommand>` — secure local vault for AI
agent OAuth tokens with instant profile switching, smart multi-account
rotation, and automatic rate-limit failover.

```
agentflare agents auth backup <agent> <profile>     # Save current auth to vault
agentflare agents auth activate <agent> <profile>   # Restore auth (<100ms switch)
agentflare agents auth status [agent]               # Show active profile (content-hash)
agentflare agents auth ls [agent]                   # List saved profiles
agentflare agents auth clear <agent>                # Remove auth files (logout state)
agentflare agents auth delete <agent> <profile>     # Remove profile from vault
agentflare agents auth pick <agent>                 # Interactive profile selector (fzf)
agentflare agents auth rename <agent> <old> <new>   # Rename profile (non-destructive)
agentflare agents auth alias <agent> <profile> <alias> # Create short alias

agentflare agents auth rotate <agent>               # Smart multi-account rotation
agentflare agents auth cooldown set <agent>[/<profile>] [--minutes N]
agentflare agents auth cooldown list|clear
agentflare agents auth next <agent>                 # Preview what rotation would pick
agentflare agents auth run <agent> -- [args...]     # Wrap CLI with auto-failover

agentflare agents auth project set <agent> <profile> # Link profile to cwd
agentflare agents auth project unset <agent>         # Remove project association

agentflare agents auth isolate add <agent> <profile>  # Create isolated $HOME profile
agentflare agents auth isolate ls [agent]
agentflare agents auth isolate delete <agent> <profile>
agentflare agents auth exec <agent> <profile> -- [args...]  # Run with isolated profile
agentflare agents auth login <agent> <profile>      # Login flow for isolated profile
```

## Design

### Auth file catalog (per agent)

| Agent | Auth files |
|-------|-----------|
| Claude Code | `~/.claude/.credentials.json`, `~/.claude.json`, `~/.config/claude-code/auth.json`, (macOS) `~/Library/Application Support/Claude/config.json` |
| Codex CLI | `~/.codex/auth.json` (respects `$CODEX_HOME`) |
| Antigravity CLI | `~/.gemini/antigravity-cli/antigravity-oauth-token`, `~/.gemini/google_accounts.json` |
| Gemini CLI (legacy) | `~/.gemini/settings.json`, `~/.gemini/oauth_creds.json` |
| OpenCode | `~/.opencode/auth.json` |
| Copilot CLI | `~/.copilot/auth.json` |

### Vault storage

```
~/.local/share/agentflare/vault/
├── claude/
│   ├── alice@gmail.com/
│   │   ├── .credentials.json
│   │   ├── .claude.json          (partial — oauth fields only)
│   │   └── auth.json
│   └── bob@gmail.com/
│       └── ...
├── codex/
│   ├── work@company.com/
│   │   └── auth.json
│   └── personal@gmail.com/
│       └── auth.json
└── agy/
    └── ...
```

Vault itself encrypted via [secrets-vault](https://crates.io/crates/secrets-vault)
(AES-256-GCM + PBKDF2, 4 deps, 508KB). Master passphrase from env var
`AGENTFLARE_VAULT_PASSPHRASE` or interactive prompt.

### Content-hash profile detection

`status` detects active profile without any sidecar state:
1. SHA-256 hash current live auth files
2. Compare against all vault profile hashes
3. Match = that profile is active

Survives reboots, detects manual switches, no desync possible.

### Multi-account rotation

Three algorithms:

| Algorithm | Behavior |
|-----------|----------|
| **smart** (default) | Multi-factor: health + recency + cooldown + plan tier + random jitter |
| **round-robin** | Sequential, skips cooldown profiles |
| **random** | Uniform random among non-cooldown |

### Profile health scoring

Each profile gets a health indicator:

| Status | Meaning |
|--------|---------|
| healthy | Token valid >1h, no recent errors |
| warning | Token expiring within 1h, or minor issues |
| critical | Token expired, or repeated errors in last hour |
| unknown | No health data yet |

Penalty system: exponential decay (20% every 5 min). After ~30 min of
no errors, penalty returns to near zero.

### Cooldown tracking

```bash
agentflare agents auth cooldown set claude            # 60 min default
agentflare agents auth cooldown set codex/work --minutes 120
agentflare agents auth cooldown list
agentflare agents auth cooldown clear claude/alice
```

Rotation algorithms automatically skip profiles in cooldown. Activating
a cooldown'd profile produces a warning + confirmation prompt (bypass with
`--force`).

### Automatic failover (`agents auth run`)

```
agentflare agents auth run claude -- "explain this code"
```

If Claude hits a rate limit mid-session:
1. Current profile goes into cooldown
2. Next best profile selected via rotation
3. Command re-executed with new auth

Shell alias: `alias claude='agentflare agents auth run claude --'`

### Profile isolation (parallel sessions)

**Isolated profiles:** Full `$HOME` isolation per profile. Real `.ssh`,
`.gitconfig`, etc. symlinked from host. For concurrent multi-account
sessions in different terminals.

**Shallow profiles:** Only auth-bearing files are real, everything else
symlinks back to real `~/`. For orchestrators fanning N parallel agent
sessions across N accounts. `$HOME` repointed at shallow profile,
provider home var pinned (`$CODEX_HOME`, `$GEMINI_HOME`).

### Project-profile associations

```bash
cd ~/projects/work-app
agentflare agents auth project set claude work@company.com
# Now `agentflare agents auth activate claude` auto-resolves to work@company.com
```

Cascading: parent directory associations apply to subdirectories unless
a more specific association exists.

### Daemon awareness

Codex runs as a long-lived daemon (`codex app-server`, `codex mcp-server`)
that caches `auth.json` in memory at startup. Swapping auth files on disk
does NOT change the daemon's account until restart.

`activate` detects a running daemon and prints a warning. `--reload-daemon`
flag sends SIGTERM to the daemon (it respawns with new auth on next use).
Never kills a daemon silently.

### Agent-optimized output

`--json` on all commands: stdout = structured data, stderr = diagnostics,
exit code = result. Designed for agent consumption.

## Phases

### Phase 1 — Vault + basic switching (3 days)
- `auth backup`, `auth activate`, `auth status`, `auth ls`, `auth clear`, `auth delete`
- Auth file catalog for claude, codex, antigravity, gemini
- Content-hash profile detection
- Vault encryption (secrets-vault or equivalent)
- `--json` output

### Phase 2 — Rotation + cooldown (2 days)
- `auth rotate` with all 3 algorithms
- `auth cooldown set|list|clear`
- Profile health scoring + exponential decay penalty
- `auth next` preview
- `auth pick` interactive selector

### Phase 3 — Failover + isolation (3 days)
- `auth run` wrapper with automatic failover
- Daemon detection + `--reload-daemon`
- `auth isolate add|ls|delete`
- `auth exec` with isolated profile
- `auth login` for isolated profile
- Shallow profile spawning for orchestrators

### Phase 4 — Extras (1 day)
- `auth rename`, `auth alias`
- `auth project set|unset`
- Extend catalog to opencode, copilot, remaining agents

## Prior art

Direct inspiration from [coding_agent_account_manager](https://github.com/Dicklesworthstone/coding_agent_account_manager)
(caam):

| Feature | CAAM command | agentflare equivalent |
|---------|-------------|----------------------|
| Backup auth | `caam backup <tool> <email>` | `agents auth backup <agent> <profile>` |
| Instant switch | `caam activate <tool> <email>` | `agents auth activate <agent> <profile>` |
| Active profile | `caam status` | `agents auth status` |
| List profiles | `caam ls` | `agents auth ls` |
| Auto-rotate | `caam activate <tool> --auto` | `agents auth rotate <agent>` |
| Cooldown | `caam cooldown set <tool>` | `agents auth cooldown set <agent>` |
| Failover wrapper | `caam run <tool> -- ...` | `agents auth run <agent> -- ...` |
| Isolated profiles | `caam profile add/exec` | `agents auth isolate add/exec` |
| Project association | `caam project set` | `agents auth project set` |
| Interactive pick | `caam pick` | `agents auth pick` |

## Scope boundaries

In scope:
- Auth file backup/restore per agent+profile
- Content-hash active profile detection
- Encrypted local vault (passphrase + env var)
- Multi-account rotation (smart/round-robin/random)
- Cooldown tracking with exponential decay
- Automatic failover on rate limit
- Profile isolation (isolated + shallow modes)
- Daemon detection + controlled restart
- Project-profile associations
- JSON output for agent consumption

Out of scope (v1):
- Usage/cost tracking per profile
- Token refresh (agents manage their own refresh)
- Cloud-based vault sync
- Team/multi-user profile sharing
- Browser-based login orchestration

## Success criteria

- [ ] `auth activate` switches auth files in <100ms
- [ ] Content-hash detection matches profiles after manual switches and reboots
- [ ] Rotation skips cooldown profiles, respects health scores
- [ ] `auth run` auto-failover handles at least 3 retries across profiles
- [ ] Isolated profiles don't leak auth between concurrent sessions
- [ ] Vault is encrypted at rest (AES-256-GCM minimum)
- [ ] Daemon detection works for codex app-server
- [ ] All commands support `--json`
