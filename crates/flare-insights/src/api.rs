//! Minimal 127.0.0.1 HTTP API for claude+opencode insights (no new deps, tokio only)
//! Endpoints: GET /api/health, /api/sessions, /api/sessions/:id, /api/search?q=, /api/stats
//! Adopted from agent-trail / agentsview REST shapes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::config::InsightsConfig;
use crate::store::InsightsStore;

pub const DEFAULT_PORT: u16 = 3456;

pub async fn serve(db_path: PathBuf, port: u16) -> anyhow::Result<()> {
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    println!("flare-insights API on http://{} (127.0.0.1 only)", addr);
    println!("  GET /api/health");
    println!("  GET /api/sessions?limit=20&offset=0&source=claude_code");
    println!("  GET /api/sessions/:id");
    println!("  GET /api/search?q=hello&limit=20");
    println!("  GET /api/stats");

    // DRY: spawn watcher in background to keep DB fresh
    let db_clone = db_path.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            let config = InsightsConfig::default();
            if let Ok(store) = InsightsStore::open(&db_clone) {
                let _ = crate::ingest::watcher::InsightsWatcher::rescan_and_store(&config, &store);
            }
        }
    });

    loop {
        let (mut socket, _) = listener.accept().await?;
        let db_path = db_path.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(&mut socket, &db_path).await {
                eprintln!("api error: {e}");
            }
        });
    }
}

async fn handle_conn(
    socket: &mut tokio::net::TcpStream,
    db_path: &Path,
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 8192];
    let n = socket.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let (method, path) = parse_request(&req);

    if method != "GET" {
        return write_response(socket, 405, "Method Not Allowed", &serde_json::json!({"error":"method not allowed"})).await;
    }

    let store = match InsightsStore::open(db_path) {
        Ok(s) => s,
        Err(e) => {
            return write_response(socket, 500, "DB error", &serde_json::json!({"error": e.to_string()})).await
        }
    };

    let (route, query) = split_query(path);

    let (status, body) = match route {
        "/api/health" => (200, serde_json::json!({"status":"ok","version": crate::VERSION})),
        "/api/sessions" => {
            let params = parse_query(query);
            let limit = params.get("limit").and_then(|v| v.parse().ok()).unwrap_or(20);
            let offset = params.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
            let source = params.get("source").cloned();
            let mut sessions = store.list_sessions(limit, offset).unwrap_or_default();
            if let Some(src) = source {
                sessions.retain(|s| s.source.as_str() == src);
            }
            serde_json::to_value(&sessions).unwrap()
                .as_array()
                .map(|arr| (200, serde_json::Value::Array(arr.clone())))
                .unwrap_or((200, serde_json::json!([])))
        }
        p if p.starts_with("/api/sessions/") => {
            let id = p.trim_start_matches("/api/sessions/").trim();
            match store.get_session(id).unwrap_or(None) {
                Some(s) => {
                    let turns = store.get_turns(&s.id).unwrap_or_default();
                    let tools = store.get_tool_calls(&s.id).unwrap_or_default();
                    let files = store.get_file_events(&s.id).unwrap_or_default();
                    (200, serde_json::json!({"session": s, "turns": turns, "tool_calls": tools, "file_events": files}))
                }
                None => (404, serde_json::json!({"error":"not found"})),
            }
        }
        "/api/search" => {
            let params = parse_query(query);
            let q = params.get("q").cloned().unwrap_or_default();
            let limit = params.get("limit").and_then(|v| v.parse().ok()).unwrap_or(20);
            let opts = crate::search::SearchOptions {
                query: q,
                source: params.get("source").cloned(),
                project: params.get("project").cloned(),
                limit,
                offset: 0,
                include_files: true,
                include_tools: true,
            };
            let res = crate::search::search(&store, &opts).unwrap_or_default();
            (200, serde_json::to_value(&res).unwrap())
        }
        "/api/stats" => {
            let sessions = store.list_sessions(10000, 0).unwrap_or_default();
            let tools = store.list_tool_calls(100000).unwrap_or_default();
            let files = store.list_file_events(100000).unwrap_or_default();
            let analytics = crate::analytics::compute_analytics_with_tools(&sessions, &tools, &files);
            (200, serde_json::to_value(&analytics).unwrap())
        }
        _ => (404, serde_json::json!({"error":"not found", "hint":"/api/health, /api/sessions, /api/sessions/:id, /api/search?q=, /api/stats"})),
    };

    let status_text = match status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    };
    write_response(socket, status, status_text, &body).await
}

fn parse_request(req: &str) -> (&str, &str) {
    let mut lines = req.lines();
    if let Some(first) = lines.next() {
        let mut parts = first.split_whitespace();
        let method = parts.next().unwrap_or("GET");
        let path = parts.next().unwrap_or("/");
        return (method, path);
    }
    ("GET", "/")
}

fn split_query(path: &str) -> (&str, Option<&str>) {
    if let Some(idx) = path.find('?') {
        (&path[..idx], Some(&path[idx + 1..]))
    } else {
        (path, None)
    }
}

fn parse_query(query: Option<&str>) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Some(q) = query {
        for pair in q.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                m.insert(
                    urlencoding::decode(k).unwrap_or(k.into()).into_owned(),
                    urlencoding::decode(v).unwrap_or(v.into()).into_owned(),
                );
            }
        }
    }
    m
}

async fn write_response(
    socket: &mut tokio::net::TcpStream,
    status: u16,
    status_text: &str,
    body: &serde_json::Value,
) -> anyhow::Result<()> {
    let body_str = serde_json::to_string(body)?;
    let resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
        status,
        status_text,
        body_str.len(),
        body_str
    );
    socket.write_all(resp.as_bytes()).await?;
    socket.flush().await?;
    Ok(())
}

// Minimal urlencoding fallback if crate not present
mod urlencoding {
    pub fn decode(s: &str) -> Result<std::borrow::Cow<'_, str>, ()> {
        // very small: only decode %20 etc.
        if !s.contains('%') {
            return Ok(std::borrow::Cow::Borrowed(s));
        }
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '%' {
                let hi = chars.next().unwrap_or('0');
                let lo = chars.next().unwrap_or('0');
                let hex = format!("{}{}", hi, lo);
                if let Ok(b) = u8::from_str_radix(&hex, 16) {
                    out.push(b as char);
                }
            } else if c == '+' {
                out.push(' ');
            } else {
                out.push(c);
            }
        }
        Ok(std::borrow::Cow::Owned(out))
    }
}
