# Claude proxy wrapper

Approved in chat: one-time `agentflare wrap claude` setup, normal `claude` passthrough, opt-in `claude --with-proxy --resume`, and `agentflare unwrap claude` removal. `claude-code` is accepted as an alias, matching lean-ctx; the registry retains its existing identifier.

Use a managed shell wrapper rather than replacing Claude's executable. Reuse agentflare's shell detection/profile paths and atomic writer. PowerShell uses a native alias to an owned agentflare companion to preserve `$?`, `$LASTEXITCODE` and Claude's `-p`; other shells use a function. The wrapper dispatches to an internal launch path, which resolves the real executable on PATH and preserves cwd, stdio, argument boundaries and exit status. Only a leading `--with-proxy` is a wrapper option; arguments after Claude options or `--` are native Claude arguments.

Opt-in HTTPS uses the tested local TLS bridge and the single default lean-ctx optimizer on port 4444. The optimizer's normal data directories retain statistics across projects and sessions; exiting Claude stops its bridge and leaves the shared optimizer alive. No provider base URL or system certificate trust changes. Existing authentication is preserved. No automatic paid model probes.

Approaches: persistent provider-base-URL changes do not implement the requested HTTPS route; a separate `claude-https` alias has already proved the route but requires another command name. The approved shell function offers the requested interface without changing the vendor binary. Native Rust wrapper management embeds the existing Python bridge behavior; Python with cryptography and lean-ctx are required only for proxy launches.

Initial support is Claude Code in PowerShell/Bash/Zsh/Fish. Codex and Cursor remain outside this change pending their transport validation. The HTTP/1.1 bridge remains experimental, with a 2 MB request limit, temporary process-scoped CA trust, and child processes inheriting proxy variables. Refuse conflicting unmanaged Claude functions/aliases or malformed managed blocks; unwrap removes only its own block. This change does not claim subscription cash savings.
