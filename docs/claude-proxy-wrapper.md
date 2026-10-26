# Claude shell wrapper

`agentflare wrap claude` installs a managed shell wrapper. `claude-code` is an accepted alias for `claude`. The Claude executable is not replaced. Supported shells are PowerShell, Bash, Zsh and Fish. PowerShell uses a native alias and an owned companion executable so native arguments, `$?` and `$LASTEXITCODE` retain their normal behavior; other shells use a function.

```sh
agentflare wrap claude
# Reload the profile using the command printed by wrap, or open a new terminal.
claude --resume
claude --with-proxy --resume
agentflare unwrap claude
```

Plain `claude` passes all arguments to the real executable. Put the optional `--with-proxy` first. After that, Claude arguments keep their original order and quoting. A native prompt such as `claude -p --with-proxy` is not interpreted as a wrapper flag.

Use `--shell bash|zsh|fish|powershell` to select a shell and `--profile <path>` for a custom profile. Remove a custom-profile wrapper with the same flags. Existing unrelated profile content is preserved; conflicting Claude definitions and malformed managed blocks require manual resolution.

## HTTPS proxy

Proxy launches require lean-ctx on PATH and Python 3.10+ with `cryptography` installed. They start the default lean-ctx proxy on port 4444 if needed, then reuse it across projects and terminals. No `proxy enable`, provider base URL wiring, shell-wide proxy exports or OS certificate installation is required.

The route is Claude → temporary loopback CONNECT/TLS bridge → default lean-ctx proxy → Anthropic. Only `api.anthropic.com` is intercepted; other HTTPS destinations remain opaque tunnels. Claude's existing authentication is retained. Exiting Claude removes its temporary bridge and certificates; the shared optimizer keeps running and keeps its normal statistics.

```sh
lean-ctx proxy status
lean-ctx savings --period day
lean-ctx savings --period day --format json
```

`proxy status` reports request/compression counts and estimated tokens removed by upstream. `savings` also includes MCP/read/search/shell results, so it is not a proxy-only total. Cache reuse differs from token removal. Dollar figures are API-equivalent estimates, not cash refunded on a subscription.

The bridge is experimental: HTTP/1.1, a 2 MB request limit, one-day certificates, and proxy environment variables inherited by child tools. WebSockets/Remote Control, Codex and Cursor are not covered. An explicitly configured custom `ANTHROPIC_BASE_URL` is rejected rather than silently redirected. Use normal Claude for custom providers.
