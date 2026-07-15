//! HTTP client for the official MCP Registry
//! (registry.modelcontextprotocol.io/v0.1/servers).
//! Used as a fallback when local BM25 search returns no results or too few.

use crate::search::InstallHint;
use serde::Deserialize;
use std::time::Duration;

const REGISTRY_BASE: &str = "https://registry.modelcontextprotocol.io/v0.1/servers";

/// Wall-clock budget for the fallback request to the official MCP
/// Registry. Deliberately short -- this is a best-effort fallback path,
/// not a critical dependency, and callers rely on it never blocking a
/// search for longer than this regardless of network conditions.
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(3);

/// A server entry from the registry's JSON response.
#[derive(Debug, Deserialize)]
struct RegistryResponse {
    servers: Vec<RegistryServerEntry>,
}

#[derive(Debug, Deserialize)]
struct RegistryServerEntry {
    server: RegistryServer,
}

#[derive(Debug, Deserialize)]
struct RegistryServer {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    repository: Option<RegistryRepository>,
    #[serde(default)]
    packages: Vec<RegistryPackage>,
    #[serde(default)]
    remotes: Vec<RegistryRemote>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RegistryRepository {
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RegistryPackage {
    #[serde(rename = "registryType")]
    registry_type: String,
    identifier: String,
    #[serde(default)]
    runtime_hint: Option<String>,
    #[allow(dead_code)]
    #[serde(rename = "transport")]
    _transport: Option<RegistryTransport>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RegistryTransport {
    #[serde(rename = "type")]
    transport_type: String,
}

#[derive(Debug, Deserialize)]
struct RegistryRemote {
    #[serde(rename = "type")]
    remote_type: String,
    url: String,
}

/// A registry search hit — a server whose name/description matched the query.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RegistryHit {
    pub server: String,
    pub description: String,
    pub install_hint: Option<InstallHint>,
    pub remote_url: Option<String>,
}

/// Picks the remote URL agentflare can actually connect to. The registry
/// may list multiple remotes with different transports (e.g. the
/// deprecated HTTP+SSE alongside the current standard); only
/// "streamable-http" is one this codebase speaks, so a blind first()/next()
/// risks surfacing a URL nothing here can use.
fn select_remote_url(remotes: Vec<RegistryRemote>) -> Option<String> {
    remotes
        .into_iter()
        .find(|r| r.remote_type == "streamable-http")
        .map(|r| r.url)
}

/// Search the official MCP Registry for servers matching `query`.
/// Returns up to `limit` results. A network error returns `Ok(vec![])` so
/// local search is never degraded by a transient registry outage.
pub fn search_registry(query: &str, limit: usize) -> Vec<RegistryHit> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let url = format!("{REGISTRY_BASE}?search={}&version=latest", urlencode(query));
    let resp = match ureq::get(&url).timeout(REGISTRY_TIMEOUT).call() {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let body: RegistryResponse = match resp.into_json() {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    body.servers
        .into_iter()
        .take(limit)
        .map(|entry| {
            let server = entry.server;
            let description = server
                .title
                .clone()
                .or(server.description.clone())
                .unwrap_or_default();
            let install_hint = server.packages.into_iter().next().map(|p| InstallHint {
                registry_type: p.registry_type,
                identifier: p.identifier,
                runtime_hint: p.runtime_hint,
            });
            let remote_url = select_remote_url(server.remotes);
            RegistryHit {
                server: server.name,
                description,
                install_hint,
                remote_url,
            }
        })
        .collect()
}

fn urlencode(s: &str) -> String {
    s.as_bytes()
        .iter()
        .map(|&b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{:02X}", b),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(remote_type: &str, url: &str) -> RegistryRemote {
        RegistryRemote {
            remote_type: remote_type.to_string(),
            url: url.to_string(),
        }
    }

    #[test]
    fn select_remote_url_prefers_streamable_http() {
        let remotes = vec![
            remote("sse", "https://example.com/sse"),
            remote("streamable-http", "https://example.com/mcp"),
        ];
        assert_eq!(
            select_remote_url(remotes),
            Some("https://example.com/mcp".to_string())
        );
    }

    #[test]
    fn select_remote_url_returns_none_when_no_streamable_http_remote() {
        let remotes = vec![remote("sse", "https://example.com/sse")];
        assert_eq!(select_remote_url(remotes), None);
    }
    #[test]
    fn empty_query_returns_empty() {
        assert!(search_registry("  ", 5).is_empty());
    }

    #[test]
    fn urlencodes_special_chars() {
        let encoded = urlencode("hello world");
        assert_eq!(encoded, "hello%20world");
    }

    #[test]
    fn registry_base_uses_the_stable_v0_1_endpoint() {
        assert!(
            REGISTRY_BASE.ends_with("/v0.1/servers"),
            "registry.modelcontextprotocol.io only serves /v0.1/servers -- /v0/servers is not a real endpoint: {REGISTRY_BASE}"
        );
    }
}
