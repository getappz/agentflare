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
    HttpApi {
        base_url: String,
        #[serde(default)]
        auth_ref: Option<String>,
        #[serde(default)]
        tools: Vec<HttpToolConfig>,
    },
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct HttpToolConfig {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub method: String,
    pub path: String,
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
        if let ServerConfig::McpStdio { auth_ref, auth_env, .. } = server {
            if auth_ref.is_some() != auth_env.is_some() {
                return Err(ConfigError::IncompleteAuthConfig {
                    server: name.clone(),
                    auth_ref: auth_ref.clone(),
                    auth_env: auth_env.clone(),
                });
            }
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
    fn parses_http_api_server_with_tools() {
        let cfg = parse(
            r#"
            [servers.weather]
            kind = "http_api"
            base_url = "https://api.weather.com"
            auth_ref = "weather_api_key"
            [[servers.weather.tools]]
            name = "get_forecast"
            description = "Get weather forecast for a city"
            method = "GET"
            path = "/v1/forecast"
            "#,
        )
        .unwrap();
        let ServerConfig::HttpApi { base_url, tools, .. } = cfg.servers.get("weather").unwrap()
        else {
            panic!("expected HttpApi");
        };
        assert_eq!(base_url, "https://api.weather.com");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "get_forecast");
        assert_eq!(tools[0].method, "GET");
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
}
