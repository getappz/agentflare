// During `init`, detect project context (e.g. a GitHub remote) and, with the
// user's OK, register the matching MCP server BEHIND agentflare's own gateway
// (`~/.agentflare/gateway.toml`) — so its tools stay reachable through
// `tool_search`/`tool_execute` instead of bloating the host's always-on
// tool list. Adding another gateway-fronted MCP later is one more entry in
// `INTEGRATIONS`; the plumbing (detect → consent → idempotent append) is shared.
use crate::paths::home;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub struct GatewayIntegration {
    /// gateway.toml server key — also the idempotency key.
    pub name: &'static str,
    /// Is this integration relevant to the current project? (host-independent)
    pub detect: fn() -> bool,
    /// One-line reason shown before the consent prompt.
    pub prompt: &'static str,
    /// The `[servers.<name>]` block appended to gateway.toml (valid TOML,
    /// leading/trailing whitespace is normalized on write).
    pub toml_block: &'static str,
    /// Follow-up lines (e.g. how to store the auth token) printed after a
    /// successful registration.
    pub post_note: fn() -> Vec<String>,
}

pub const INTEGRATIONS: &[GatewayIntegration] = &[GITHUB, LEANCTX];

const GITHUB: GatewayIntegration = GatewayIntegration {
    name: "github",
    detect: git_remote_is_github,
    prompt: "⚑ GitHub repo detected. github-mcp-server can sit behind the agentflare gateway\n  (its tools stay under tool_search/tool_execute, not the host's tool list).",
    // Remote HTTP backend — zero-install (no docker/binary). The gateway
    // sends `auth_header` verbatim, so the stored secret is the full header
    // value (`Bearer <token>`), see `post_note`.
    toml_block: "[servers.github]\nkind = \"mcp_http\"\nurl = \"https://api.githubcopilot.com/mcp/\"\nauth_ref = \"github_token\"\nauth_env = \"GITHUB_TOKEN\"\nauth_header = \"Authorization\"",
    post_note: github_post_note,
};

fn github_post_note() -> Vec<String> {
    let bin = crate::paths::agentflare_binary();
    vec![
        format!("  next  store your token:  {bin} gateway secret set github_token"),
        "  next    then paste:  Bearer ghp_<your-token>   ('Bearer ' prefix required)".to_string(),
    ]
}

/// lean-ctx's own installer/`onboard` wires its ~80 `ctx_*` tools straight
/// into the host's native MCP config — exactly the always-on tool-list bloat
/// this gateway exists to avoid. Once the binary is on PATH, this puts it
/// behind the gateway instead; `components.rs`'s `"leanctx"` component also
/// strips whatever native registration the upstream onboarder already
/// created, so the same tools don't end up declared twice.
pub const LEANCTX: GatewayIntegration = GatewayIntegration {
    name: "leanctx",
    detect: leanctx_installed,
    prompt: "⚑ lean-ctx detected. Its ~80 ctx_* tools can sit behind the agentflare gateway\n  (reachable via tool_search/tool_execute) instead of bloating the host's tool list.",
    // Local stdio backend — same binary lean-ctx's own installer already put
    // on PATH; the gateway just spawns it instead of the host declaring it
    // natively. No auth needed (local process).
    toml_block: "[servers.leanctx]\nkind = \"mcp_stdio\"\ncommand = \"lean-ctx\"\nargs = []",
    post_note: leanctx_post_note,
};

fn leanctx_installed() -> bool {
    crate::tool_install::installed(&crate::tool_install::LEAN_CTX)
}

fn leanctx_post_note() -> Vec<String> {
    vec![
        "  next  its ctx_* tools are now reached via tool_search/tool_execute, not called natively"
            .to_string(),
    ]
}

pub fn gateway_toml_path() -> PathBuf {
    home().join(".agentflare").join("gateway.toml")
}

/// A remote that mentions "github" — matches `github.com` in HTTPS/SSH URLs
/// and SSH host aliases like `git@github-work:org/repo.git`. Deliberately
/// broad: a rare false positive (e.g. a non-GitHub remote whose path contains
/// "github") only leads to the consent prompt, which the user can decline —
/// preferred over missing a legitimately-GitHub SSH alias.
fn remotes_mention_github(remotes: &str) -> bool {
    remotes.to_lowercase().contains("github")
}

fn git_remote_is_github() -> bool {
    Command::new("git")
        .args(["remote", "-v"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| remotes_mention_github(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(false)
}

/// True if a server named `name` is already configured in gateway.toml — the
/// idempotency gate. Uses the same parser the gateway itself uses, so a
/// present-but-differently-formatted entry still counts as registered.
pub fn already_registered(name: &str) -> bool {
    fs::read_to_string(gateway_toml_path())
        .ok()
        .and_then(|s| gateway_registry::parse_config(&s).ok())
        .map(|cfg| cfg.servers.contains_key(name))
        .unwrap_or(false)
}

/// Appends the integration's block to gateway.toml (creating the file if
/// needed), preserving any existing servers. Self-guarding: if the server is
/// already registered it no-ops rather than appending a duplicate table (which
/// would be invalid TOML), so it's safe to call directly, not only behind the
/// caller's `already_registered` check. Returns a status line.
pub fn register(intg: &GatewayIntegration) -> String {
    let path = gateway_toml_path();
    if already_registered(intg.name) {
        return format!(
            "skip  {} already registered ({})",
            intg.name,
            path.display()
        );
    }
    let existing = fs::read_to_string(&path).unwrap_or_default();

    let mut out = existing.trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(intg.toml_block.trim());
    out.push('\n');

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::write(&path, out) {
        Ok(_) => format!(
            "ok    {} MCP registered behind the gateway ({})",
            intg.name,
            path.display()
        ),
        Err(e) => format!("fail  writing {}: {e}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn remotes_mention_github_matches_https_ssh_and_aliases() {
        assert!(remotes_mention_github(
            "origin\thttps://github.com/org/repo.git (fetch)"
        ));
        assert!(remotes_mention_github(
            "origin\tgit@github.com:org/repo.git (push)"
        ));
        // SSH host alias (this very repo's shape).
        assert!(remotes_mention_github(
            "origin\tgit@github-work:org/repo.git (fetch)"
        ));
    }

    #[test]
    fn remotes_mention_github_ignores_other_forges() {
        assert!(!remotes_mention_github(
            "origin\tgit@gitlab.com:org/repo.git (fetch)"
        ));
        assert!(!remotes_mention_github(""));
    }

    #[test]
    fn register_writes_a_valid_parseable_github_server() {
        with_temp_home(|| {
            assert!(!already_registered("github"));
            let msg = register(&GITHUB);
            assert!(msg.starts_with("ok"), "unexpected: {msg}");

            let content = fs::read_to_string(gateway_toml_path()).unwrap();
            assert!(content.contains("[servers.github]"));
            let cfg = gateway_registry::parse_config(&content).unwrap();
            assert!(cfg.servers.contains_key("github"));
            assert!(already_registered("github"));
        });
    }

    #[test]
    fn register_is_idempotent_and_never_duplicates() {
        with_temp_home(|| {
            let msg1 = register(&GITHUB);
            assert!(msg1.starts_with("ok"), "first: {msg1}");
            let first = fs::read_to_string(gateway_toml_path()).unwrap();

            // A second direct call must no-op, not append a second
            // [servers.github] table (which would be invalid TOML).
            let msg2 = register(&GITHUB);
            assert!(msg2.starts_with("skip"), "second: {msg2}");
            let second = fs::read_to_string(gateway_toml_path()).unwrap();

            assert_eq!(first, second, "second register must not change the file");
            assert_eq!(second.matches("[servers.github]").count(), 1);
            assert!(gateway_registry::parse_config(&second).is_ok());
        });
    }

    #[test]
    fn register_preserves_an_existing_server() {
        with_temp_home(|| {
            let path = gateway_toml_path();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                &path,
                "[servers.acme]\nkind = \"mcp_stdio\"\ncommand = \"acme\"\n",
            )
            .unwrap();

            register(&GITHUB);

            let content = fs::read_to_string(&path).unwrap();
            let cfg = gateway_registry::parse_config(&content).unwrap();
            assert!(cfg.servers.contains_key("acme"));
            assert!(cfg.servers.contains_key("github"));
        });
    }

    #[test]
    fn register_writes_a_valid_parseable_leanctx_server() {
        with_temp_home(|| {
            assert!(!already_registered("leanctx"));
            let msg = register(&LEANCTX);
            assert!(msg.starts_with("ok"), "unexpected: {msg}");

            let content = fs::read_to_string(gateway_toml_path()).unwrap();
            assert!(content.contains("[servers.leanctx]"));
            let cfg = gateway_registry::parse_config(&content).unwrap();
            assert!(cfg.servers.contains_key("leanctx"));
            assert!(already_registered("leanctx"));
        });
    }

    #[test]
    fn register_leanctx_is_idempotent_and_never_duplicates() {
        with_temp_home(|| {
            let msg1 = register(&LEANCTX);
            assert!(msg1.starts_with("ok"), "first: {msg1}");
            let msg2 = register(&LEANCTX);
            assert!(msg2.starts_with("skip"), "second: {msg2}");
            let content = fs::read_to_string(gateway_toml_path()).unwrap();
            assert_eq!(content.matches("[servers.leanctx]").count(), 1);
        });
    }

    #[test]
    fn integrations_list_includes_github_and_leanctx() {
        let names: Vec<&str> = INTEGRATIONS.iter().map(|i| i.name).collect();
        assert!(names.contains(&"github"));
        assert!(names.contains(&"leanctx"));
    }
}
