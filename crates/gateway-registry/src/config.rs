//! Parses `~/.agentflare/gateway.toml`. `kind` is the seam for future
//! backend types — `execute` dispatches on whatever `Backend` variant a
//! `ServerConfig` builds into (see `backend.rs`), regardless of `kind`.

use std::collections::HashMap;

#[derive(Debug, Default, serde::Deserialize)]
pub struct GatewayConfig {
    #[serde(default)]
    pub servers: HashMap<String, ServerConfig>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerConfig {
    McpStdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        auth_ref: Option<String>,
        #[serde(default)]
        auth_env: Option<String>,
    },
    McpHttp {
        url: String,
        #[serde(default)]
        auth_ref: Option<String>,
        #[serde(default)]
        auth_env: Option<String>,
        /// Which HTTP header the resolved `auth_ref` secret becomes.
        /// Defaults to `"Authorization"` when the server actually builds a
        /// backend (see `McpHttpBackend::new` in Task 6) — left `None` here
        /// rather than defaulted at parse time, so a config that sets
        /// neither `auth_ref` nor `auth_header` round-trips as `None`
        /// instead of a misleading `Some("Authorization")` no header is
        /// ever actually sent for.
        #[serde(default)]
        auth_header: Option<String>,
    },
}

/// `parse`'s error type: either a TOML syntax/shape error, or a config that
/// parses fine but fails a semantic check `serde` can't express on its own
/// (currently just the `auth_ref`/`auth_env` pairing below).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(transparent)]
    Toml(#[from] toml::de::Error),
    #[error(
        "server '{server}': auth_ref and auth_env must both be set or both omitted \
         (got auth_ref={auth_ref:?}, auth_env={auth_env:?}) — a lone auth_ref silently \
         injects no credentials at runtime, so this is rejected at parse time instead"
    )]
    IncompleteAuthConfig { server: String, auth_ref: Option<String>, auth_env: Option<String> },
}

pub fn parse(toml_str: &str) -> Result<GatewayConfig, ConfigError> {
    let cfg: GatewayConfig = toml::from_str(toml_str)?;
    for (name, server) in &cfg.servers {
        let (auth_ref, auth_env) = match server {
            ServerConfig::McpStdio { auth_ref, auth_env, .. } => (auth_ref, auth_env),
            ServerConfig::McpHttp { auth_ref, auth_env, .. } => (auth_ref, auth_env),
        };
        if auth_ref.is_some() != auth_env.is_some() {
            return Err(ConfigError::IncompleteAuthConfig {
                server: name.clone(),
                auth_ref: auth_ref.clone(),
                auth_env: auth_env.clone(),
            });
        }
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mcp_stdio_server() {
        let cfg = parse(
            r#"
            [servers.narsil]
            kind = "mcp_stdio"
            command = "narsil-mcp"
            args = ["--repos", "."]
            auth_ref = "narsil_token"
            auth_env = "NARSIL_TOKEN"
            "#,
        )
        .unwrap();
        let ServerConfig::McpStdio { command, args, auth_ref, auth_env } =
            cfg.servers.get("narsil").unwrap()
        else {
            panic!("expected McpStdio");
        };
        assert_eq!(command, "narsil-mcp");
        assert_eq!(args, &vec!["--repos".to_string(), ".".to_string()]);
        assert_eq!(auth_ref.as_deref(), Some("narsil_token"));
        assert_eq!(auth_env.as_deref(), Some("NARSIL_TOKEN"));
    }

    #[test]
    fn missing_servers_table_is_empty_not_an_error() {
        let cfg = parse("").unwrap();
        assert!(cfg.servers.is_empty());
    }

    #[test]
    fn mcp_stdio_with_auth_ref_but_no_auth_env_is_rejected() {
        let err = parse(
            r#"
            [servers.narsil]
            kind = "mcp_stdio"
            command = "narsil-mcp"
            auth_ref = "narsil_token"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::IncompleteAuthConfig { server, .. } if server == "narsil"));
    }

    #[test]
    fn mcp_stdio_with_auth_env_but_no_auth_ref_is_rejected() {
        let err = parse(
            r#"
            [servers.narsil]
            kind = "mcp_stdio"
            command = "narsil-mcp"
            auth_env = "NARSIL_TOKEN"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::IncompleteAuthConfig { server, .. } if server == "narsil"));
    }

    #[test]
    fn mcp_stdio_defaults_args_to_empty_and_auth_to_none() {
        let cfg = parse(
            r#"
            [servers.bare]
            kind = "mcp_stdio"
            command = "bare-mcp"
            "#,
        )
        .unwrap();
        let ServerConfig::McpStdio { args, auth_ref, auth_env, .. } =
            cfg.servers.get("bare").unwrap()
        else {
            panic!("expected McpStdio");
        };
        assert!(args.is_empty());
        assert!(auth_ref.is_none());
        assert!(auth_env.is_none());
    }

    #[test]
    fn parses_mcp_http_server() {
        let cfg = parse(
            r#"
            [servers.narsil]
            kind = "mcp_http"
            url = "https://narsil.example.com/mcp"
            auth_ref = "narsil_token"
            auth_env = "NARSIL_TOKEN"
            auth_header = "X-Api-Key"
            "#,
        )
        .unwrap();
        let ServerConfig::McpHttp { url, auth_ref, auth_env, auth_header } =
            cfg.servers.get("narsil").unwrap()
        else {
            panic!("expected McpHttp");
        };
        assert_eq!(url, "https://narsil.example.com/mcp");
        assert_eq!(auth_ref.as_deref(), Some("narsil_token"));
        assert_eq!(auth_env.as_deref(), Some("NARSIL_TOKEN"));
        assert_eq!(auth_header.as_deref(), Some("X-Api-Key"));
    }

    #[test]
    fn mcp_http_defaults_auth_header_and_auth_to_none() {
        let cfg = parse(
            r#"
            [servers.bare]
            kind = "mcp_http"
            url = "https://bare.example.com/mcp"
            "#,
        )
        .unwrap();
        let ServerConfig::McpHttp { auth_ref, auth_env, auth_header, .. } =
            cfg.servers.get("bare").unwrap()
        else {
            panic!("expected McpHttp");
        };
        assert!(auth_ref.is_none());
        assert!(auth_env.is_none());
        assert!(auth_header.is_none());
    }

    #[test]
    fn mcp_http_with_auth_ref_but_no_auth_env_is_rejected() {
        let err = parse(
            r#"
            [servers.narsil]
            kind = "mcp_http"
            url = "https://narsil.example.com/mcp"
            auth_ref = "narsil_token"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::IncompleteAuthConfig { server, .. } if server == "narsil"));
    }
}
