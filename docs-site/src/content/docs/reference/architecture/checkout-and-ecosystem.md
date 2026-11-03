---
title: Checkout and ecosystem
description: Repository identity, contributor and agent rules, and developer gotchas from the audited agentflare checkout.
---

## Identity

| Field | Inspected value / evidence |
|---|---|
| Remote | `https://github.com/getappz/agentflare.git` (fetch/push); `git remote -v`; no embedded credentials |
| Branch | `docs/marketing-content`; `git branch --show-current` |
| HEAD | `a4e59d748a61dba653ff55e20052b09cb79376c9`; `git log -1`; “fix(supervisor): actionable PR comments when sweep will not merge” |
| Version | `1.8.1`; [manifest](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L10); installed CLI reports the same version, but this alone does not prove byte-for-byte build identity |
| License | Apache-2.0; [Cargo.toml](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L14), [LICENSE](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/LICENSE) |
| Verification host | Windows x86_64; Rust/Cargo 1.98.0, mise 2026.9.12, aube 2.2.17 observed from version commands |
| Working tree | Unrelated untracked files existed before this work; excluded from the documented implementation and left untouched |

**Resolved contradiction:** release-generated Homebrew/Scoop metadata still
contains `MIT` literals in the inspected workflow. Those literals do not change
the root package's Apache-2.0 declaration/license file; the packaging discrepancy
is recorded here, without modifying release automation.
[Release metadata generator](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/release.yml), [license](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/LICENSE).

## Contributor and agent rules

- [AGENTS.md](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/AGENTS.md) requires lean-ctx code reads/search/shell, describes
  memory recall, optional mbx/shared dependency stores, and requires stamped
  PR creation via `agentflare pr open` rather than an unstamped manual PR.
  These are repository instructions; they are distinct from compiled policy.
- [CONTRIBUTING.md](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/CONTRIBUTING.md) provides setup, verification and CLA rules;
  [CLA.md](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/CLA.md) defines the contributor agreement.
- [.github/CODEOWNERS](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/CODEOWNERS#L1) assigns all paths to `@getappz`;
  the [PR template](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/pull_request_template.md) requests summary/test/risk
  details. Its shorter clippy command is not the complete current CI command.
- [.githooks/pre-commit](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.githooks/pre-commit#L4) blocks default-branch commits
  when installed via `core.hooksPath`, chains an existing hook, and runs a staged
  Rust LOC gate. Branch-resolution errors fail open. A file's presence alone
  does not establish that a developer installed the hook.

## Developer gotchas

1. **Migration ownership matters.** Append backend migrations; do not edit old
   ones or combine independently versioned stores into a file. Installing a WIP
   binary with new migrations can prevent an older binary opening shared data.
   [backend rule](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-backend/src/db.rs#L5),
   [separate stores](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-bin-lib/src/store.rs#L4),
   [SchemaAhead](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-db-kit/src/open.rs#L16).
2. **Default builds differ from released binaries.** `semantic` is off by default;
   `skill-overrides-sync` is also optional and not part of released builds.
   Tests additionally use fast vault KDF parameters that the vault crate rejects
   in a release build. [features](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L116),
   [release guard](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/flare-vault/src/lib.rs#L3).
3. **A test pass under cargo is not identical to nextest.** Nextest config has
   retries and kill windows, including a larger override for a pause/resume test.
   Doctests run separately in CI. [nextest config](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.config/nextest.toml#L37),
   [CI test steps](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/ci.yml#L129).
4. **Build artifacts need deliberate isolation.** Dispatched agents optionally
   prepend the mbx cargo shim, with plain Cargo fallback. CI instead uses sccache
   and rejects ambient target-directory overrides. This inspection's Cargo
   metadata resolved a user-global target directory; no build was run against it.
   [mbx](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/mbx.rs#L1), [CI guard](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/ci.yml#L250).
5. **Website deployment assumes Unix utilities.** `copy:docs` uses `rm -rf` and
   `cp -R`; do not assume this task runs in bare PowerShell. Docs are mounted at
   `/docs`; their build is separate from the embedded dashboard UI.
   [site scripts](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/site/package.json), [Astro base](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/docs-site/astro.config.mjs#L4).

The broader ecosystem visible on disk includes agent-host integrations,
downstream MCP servers, GitHub, model providers, channel integrations, browser
automation and the independent public website. No sibling repository internals,
live service availability, benchmark reproduction or release-install history
was used as evidence. Marketing performance numbers in README remain attributed
claims, not measurements made by this architecture pass.
[Host components](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/components.rs#L713), [gateway](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/gateway-registry/src/lib.rs),
[proxy](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/flare-proxy/src/lib.rs#L18), [README metrics](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/README.md#metrics).

---

Previous: [Technical deep dive](/docs/reference/architecture/internals/) | Next: [Architectural blueprint](/docs/reference/architecture/blueprint/)

