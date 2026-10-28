# Claude proxy wrapper

Approved in chat: one-time `agentflare wrap claude` setup, normal `claude` passthrough, opt-in `claude --with-proxy --resume`, and `agentflare unwrap claude` removal. `claude-code` is accepted as an alias, matching lean-ctx; the registry retains its existing identifier.

Use a managed shell wrapper rather than replacing Claude's executable. Reuse agentflare's shell detection/profile paths and atomic writer. PowerShell uses a native alias to an owned agentflare companion to preserve `$?`, `$LASTEXITCODE` and Claude's `-p`; other shells use a function. The wrapper dispatches to an internal launch path, which resolves the real executable on PATH and preserves cwd, stdio, argument boundaries and exit status. Only a leading `--with-proxy` is a wrapper option; arguments after Claude options or `--` are native Claude arguments.

`agentflare update` and `agentflare dev-install` refresh an existing PowerShell companion after verifying the main binary replacement. After upgrades through an external package manager or manual binary replacement, re-run `agentflare wrap claude` to refresh it. Active sessions keep running their existing image.

Opt-in HTTPS uses the tested local TLS bridge and the single default lean-ctx optimizer on port 4444. The optimizer's normal data directories retain statistics across projects and sessions; exiting Claude stops its bridge and leaves the shared optimizer alive. No provider base URL or system certificate trust changes. Existing authentication is preserved. No automatic paid model probes.

The HTTPS bridge runs inside agentflare using Tokio, Hyper and rustls, with rcgen generating a temporary CA and leaf certificate. lean-ctx is the only external runtime dependency for proxy launches; Python, cryptography, OpenSSL commands and global trust installation are not required. Only Anthropic CONNECT traffic is decrypted; other HTTPS connections use opaque bidirectional tunnels. Requests retain their authentication and model POSTs go to the shared lean-ctx listener; responses stream back without buffering the full SSE response.

Initial support is Claude Code in PowerShell/Bash/Zsh/Fish. Codex and Cursor remain outside this change pending their transport validation. The HTTP/1.1 bridge remains experimental, with a 2 MB request limit, temporary process-scoped CA trust, and child processes inheriting proxy variables. Refuse conflicting unmanaged Claude functions/aliases or malformed managed blocks; unwrap removes only its own block. This change does not claim subscription cash savings.
