//! Minimal 127.0.0.1 HTTP API for claude+opencode insights (no new deps, tokio only)
//! Endpoints: GET /api/health, /api/sessions, /api/sessions/:id, /api/search?q=, /api/stats
//! Adopted from agent-trail / agentsview REST shapes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::config::InsightsConfig;
use crate::store::InsightsStore;

pub const DEFAULT_PORT: u16 = 3456;

/// `/api/stats` scans every session/tool_call/file_event row, which on a
/// real history can itself take longer than a short poll interval -- so
/// this TTL must be comfortably longer than that compute, not shorter,
/// or every "cached" call still recomputes and the cache buys nothing.
const STATS_CACHE_TTL: Duration = Duration::from_secs(30);

struct AppState {
    // Separate connections for the background writer and for request reads.
    // The DB runs in WAL mode specifically so a writer doesn't block
    // readers -- funneling both through one shared connection/mutex would
    // throw that away and serialize every request behind each ~5-10s
    // background resync, which is worse than the per-request reopen this
    // replaced.
    read_store: Mutex<InsightsStore>,
    write_store: Mutex<InsightsStore>,
    stats_cache: Mutex<Option<(Instant, serde_json::Value)>>,
}

pub async fn serve(db_path: PathBuf, port: u16) -> anyhow::Result<()> {
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    println!("flare-insights API on http://{} (127.0.0.1 only)", addr);
    println!("  GET /api/health");
    println!("  GET /api/sessions?limit=20&offset=0&source=claude_code");
    println!("  GET /api/sessions/:id");
    println!("  GET /api/search?q=hello&limit=20");
    println!("  GET /api/stats");

    let state = Arc::new(AppState {
        read_store: Mutex::new(InsightsStore::open(&db_path)?),
        write_store: Mutex::new(InsightsStore::open(&db_path)?),
        stats_cache: Mutex::new(None),
    });

    // DRY: spawn watcher in background to keep DB fresh
    let state_bg = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let state = state_bg.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let config = InsightsConfig::default();
                let store = state.write_store.lock().unwrap();
                crate::ingest::watcher::InsightsWatcher::rescan_and_store(&config, &store);
            })
            .await;
        }
    });

    loop {
        let (mut socket, _) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(&mut socket, &state).await {
                eprintln!("api error: {e}");
            }
        });
    }
}

async fn handle_conn(socket: &mut tokio::net::TcpStream, state: &Arc<AppState>) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 8192];
    let n = socket.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let (method, path) = parse_request(&req);

    if method != "GET" {
        return write_response(
            socket,
            405,
            "Method Not Allowed",
            &serde_json::json!({"error":"method not allowed"}),
        )
        .await;
    }

    let (route, query) = split_query(path);

    // DRY dashboard for claude/opencode (simple HTML, no build)
    if route == "/" || route == "/dashboard" {
        let html = dashboard_html();
        return write_html_response(socket, &html).await;
    }

    let (status, body) = route_request(state, route, query);

    let status_text = match status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    };
    write_response(socket, status, status_text, &body).await
}

/// Synchronous request handling -- the store lock never has to survive
/// across an `.await` point.
fn route_request(state: &Arc<AppState>, route: &str, query: Option<&str>) -> (u16, serde_json::Value) {
    let store = state.read_store.lock().unwrap();
    match route {
        "/api/health" => (
            200,
            serde_json::json!({"status":"ok","version": crate::VERSION}),
        ),
        "/api/sessions" => {
            let params = parse_query(query);
            let limit = params
                .get("limit")
                .and_then(|v| v.parse().ok())
                .unwrap_or(20);
            let offset = params
                .get("offset")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let source = params.get("source").cloned();
            let mut sessions = store.list_sessions(limit, offset).unwrap_or_default();
            if let Some(src) = source {
                sessions.retain(|s| s.source.as_str() == src);
            }
            serde_json::to_value(&sessions)
                .unwrap()
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
                    (
                        200,
                        serde_json::json!({"session": s, "turns": turns, "tool_calls": tools, "file_events": files}),
                    )
                }
                None => (404, serde_json::json!({"error":"not found"})),
            }
        }
        "/api/search" => {
            let params = parse_query(query);
            let q = params.get("q").cloned().unwrap_or_default();
            let limit = params
                .get("limit")
                .and_then(|v| v.parse().ok())
                .unwrap_or(20);
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
            let mut cache = state.stats_cache.lock().unwrap();
            let fresh = cache
                .as_ref()
                .map(|(t, _)| t.elapsed() < STATS_CACHE_TTL)
                .unwrap_or(false);
            if !fresh {
                let sessions = store.list_sessions(10000, 0).unwrap_or_default();
                let tools = store.list_tool_calls(100000).unwrap_or_default();
                let files = store.list_file_events(100000).unwrap_or_default();
                let analytics =
                    crate::analytics::compute_analytics_with_tools(&sessions, &tools, &files);
                *cache = Some((Instant::now(), serde_json::to_value(&analytics).unwrap()));
            }
            (200, cache.as_ref().unwrap().1.clone())
        }
        _ => (
            404,
            serde_json::json!({"error":"not found", "hint":"/api/health, /api/sessions, /api/sessions/:id, /api/search?q=, /api/stats"}),
        ),
    }
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
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        status_text,
        body_str.len(),
        body_str
    );
    socket.write_all(resp.as_bytes()).await?;
    socket.flush().await?;
    Ok(())
}

async fn write_html_response(socket: &mut tokio::net::TcpStream, html: &str) -> anyhow::Result<()> {
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    socket.write_all(resp.as_bytes()).await?;
    socket.flush().await?;
    Ok(())
}

fn dashboard_html() -> String {
    r#"<!doctype html><html><head><meta charset="utf-8"><title>flare-insights — claude / opencode</title>
<style>
body{font-family:system-ui, sans-serif; max-width:1000px; margin:2rem auto; padding:0 1rem; color:#1a1a1a}
a{color:#0366d6}
code{background:#f6f6f6; padding:0.1rem 0.3rem; border-radius:4px}
.stats{display:flex; gap:1rem; flex-wrap:wrap; margin:1.5rem 0}
.stat{background:#f6f6f6; border-radius:8px; padding:0.75rem 1rem; min-width:110px}
.stat .label{font-size:0.75rem; color:#666; text-transform:uppercase; letter-spacing:0.03em}
.stat .value{font-size:1.4rem; font-weight:600}
table{width:100%; border-collapse:collapse; font-size:0.85rem}
th,td{text-align:left; padding:0.4rem 0.6rem; border-bottom:1px solid #eee}
th{color:#666; font-weight:600}
#status{font-size:0.8rem; color:#888}
</style>
</head><body>
<h1>flare-insights — Claude Code + OpenCode</h1>
<p>Local-first, 127.0.0.1 only. Sources: <code>~/.agentflare/projects</code> (claude) + <code>~/.local/share/opencode/opencode.db</code></p>
<ul>
<li><a href="/api/health">/api/health</a></li>
<li><a href="/api/sessions?limit=5">/api/sessions?limit=5</a> — recent sessions</li>
<li><a href="/api/stats">/api/stats</a> — tokens, cost, by_source</li>
<li><a href="/api/search?q=agentflare">/api/search?q=agentflare</a> — FTS + file/tool</li>
</ul>
<div class="stats" id="stats"><div class="stat"><div class="label">Loading…</div></div></div>
<h2>Recent sessions</h2>
<table id="sessions"><thead><tr><th>Project</th><th>Source</th><th>Turns</th><th>Tools</th><th>Cost</th><th>Updated</th></tr></thead><tbody></tbody></table>
<p id="status"></p>
<script>
function fmtCost(c){ return (c === null || c === undefined) ? '-' : '$' + Number(c).toFixed(2); }
function fmtTokens(n){
  n = Number(n) || 0;
  if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
  if (n >= 1e3) return (n / 1e3).toFixed(1) + 'k';
  return String(n);
}
function esc(s){
  return String(s == null ? '' : s).replace(/[&<>]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;'}[c]));
}
async function load(){
  const [sessions, stats] = await Promise.all([
    fetch('/api/sessions?limit=20').then(r => r.json()),
    fetch('/api/stats').then(r => r.json())
  ]);
  document.getElementById('stats').innerHTML = `
    <div class="stat"><div class="label">Sessions</div><div class="value">${stats.total_sessions ?? 0}</div></div>
    <div class="stat"><div class="label">Tokens</div><div class="value">${fmtTokens(stats.total_tokens)}</div></div>
    <div class="stat"><div class="label">Cost</div><div class="value">${fmtCost(stats.total_cost_usd)}</div></div>
    <div class="stat"><div class="label">Cache hit</div><div class="value">${((stats.cache_hit_rate ?? 0) * 100).toFixed(0)}%</div></div>
  `;
  document.querySelector('#sessions tbody').innerHTML = sessions.map(s => `
    <tr>
      <td>${esc(s.project)}</td>
      <td>${esc(s.source)}</td>
      <td>${s.turn_count ?? 0}</td>
      <td>${s.tool_call_count ?? 0}</td>
      <td>${fmtCost(s.cost && s.cost.total_usd)}</td>
      <td>${esc((s.updated_at || '').replace('T', ' ').slice(0, 16))}</td>
    </tr>
  `).join('');
  document.getElementById('status').textContent = 'updated ' + new Date().toLocaleTimeString();
}

load();
let timer = setInterval(load, 20000);
document.addEventListener('visibilitychange', () => {
  clearInterval(timer);
  if (document.visibilityState === 'visible') {
    load();
    timer = setInterval(load, 20000);
  }
});
</script>
</body></html>"#.to_string()
}

// Minimal urlencoding fallback if crate not present
mod urlencoding {
    pub fn decode(s: &str) -> Result<std::borrow::Cow<'_, str>, ()> {
        if !s.contains('%') && !s.contains('+') {
            return Ok(std::borrow::Cow::Borrowed(s));
        }
        // Decode into raw bytes first so percent-escaped multi-byte UTF-8
        // sequences (e.g. CJK) get reassembled correctly, then convert once.
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'%' if i + 2 < bytes.len() => {
                    if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    out.push(bytes[i]);
                    i += 1;
                }
                b'+' => {
                    out.push(b' ');
                    i += 1;
                }
                b => {
                    out.push(b);
                    i += 1;
                }
            }
        }
        Ok(std::borrow::Cow::Owned(
            String::from_utf8_lossy(&out).into_owned(),
        ))
    }
}
