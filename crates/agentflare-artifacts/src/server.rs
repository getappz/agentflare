use crate::store::ArtifactStore;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Canonical fixed port for the shared artifact server. flared serves the
/// store here so artifact URLs survive individual agent sessions; MCP
/// sessions probe it before starting their own listener.
pub const DEFAULT_PORT: u16 = 64009;

pub struct ArtifactServer {
    host: String,
    port: u16,
    #[allow(dead_code)]
    // kept alive for the server's lifetime; the listener thread holds its own clone
    store: Arc<ArtifactStore>,
}

impl ArtifactServer {
    /// Start the server on loopback. `port` 0 binds an OS-assigned free
    /// port; a nonzero port is bound exactly, erroring if unavailable.
    pub fn start(store: Arc<ArtifactStore>, port: u16) -> std::io::Result<Self> {
        Self::start_on(store, "127.0.0.1", port)
    }

    /// Start on a specific interface (e.g. "0.0.0.0" for LAN sharing).
    pub fn start_on(store: Arc<ArtifactStore>, host: &str, port: u16) -> std::io::Result<Self> {
        let listener = TcpListener::bind((host, port))?;
        let actual_port = listener.local_addr()?.port();
        let server = ArtifactServer {
            host: host.to_string(),
            port: actual_port,
            store: store.clone(),
        };

        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let store = store.clone();
                        thread::spawn(move || handle_connection(stream, &store));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(server)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn url_for(&self, id: &str) -> String {
        format!("http://{}:{}/{id}", self.host, self.port)
    }

    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

/// True when an agentflare artifact server answers on `port` (loopback).
/// Fetches the index and checks for its marker so a foreign service
/// squatting on the port is not mistaken for ours. Bounded by short
/// connect/read timeouts — safe to call on every URL handout.
pub fn probe(port: u16) -> bool {
    probe_path(port, "/")
}

/// Like [`probe`] but for an index mounted under a path prefix (flared
/// serves the store under `/artifacts/`).
pub fn probe_path(port: u16, path: &str) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let request = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let _ = stream.flush();
    let mut body = String::new();
    use std::io::Read;
    let _ = stream.take(64 * 1024).read_to_string(&mut body);
    body.contains("agentflare artifacts")
}

fn handle_connection(mut stream: TcpStream, store: &ArtifactStore) {
    let _peer = stream.peer_addr().ok();
    let buf = {
        let mut reader = BufReader::new(&stream);
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).is_err() {
            return;
        }
        let parts: Vec<&str> = request_line.split_whitespace().collect();
        if parts.len() < 2 {
            return;
        }
        let method = parts[0];
        let path = parts[1];

        let mut headers = Vec::new();
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                break;
            }
            if let Some(val) = line
                .strip_prefix("Content-Length: ")
                .or_else(|| line.strip_prefix("content-length: "))
            {
                content_length = val.trim().parse().unwrap_or(0);
            }
            headers.push(line.trim().to_string());
        }

        (
            method.to_string(),
            path.to_string(),
            content_length,
            reader.into_inner(),
        )
    };

    let (method, path, _content_length, _remaining) = buf;

    let response = match (method.as_str(), path.as_str()) {
        ("GET", "/") => index_page(store),
        ("GET", path) if path.ends_with("/live") => {
            let id = path
                .strip_suffix("/live")
                .unwrap_or("")
                .trim_start_matches('/');
            if !valid_id(id) {
                let _ = stream.write_all(not_found().0.as_bytes());
                let _ = stream.flush();
                return;
            }
            return serve_sse(&mut stream, store, id);
        }
        ("GET", path) if path.ends_with("/versions") => {
            let id = path
                .strip_suffix("/versions")
                .unwrap_or("")
                .trim_start_matches('/');
            versions_json(store, id)
        }
        ("GET", path) => {
            let rest = path.trim_start_matches('/');
            match rest.split_once("/v/") {
                Some((id, v)) => match v.parse::<u32>() {
                    Ok(n) => serve_artifact(store, id, Some(n)),
                    Err(_) => not_found(),
                },
                None => serve_artifact(store, rest, None),
            }
        }
        _ => ("HTTP/1.0 405 Method Not Allowed\r\n\r\nMethod Not Allowed".to_string(),),
    };

    let _ = stream.write_all(response.0.as_bytes());
    let _ = stream.flush();
}

/// Artifact ids are single path segments (UUIDs); anything that could
/// navigate the filesystem is rejected before it reaches a path join.
/// Public so other hosts of the store (flared's `/artifacts` routes)
/// apply the same rejection before `ArtifactStore` path joins.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && !id.contains(['/', '\\']) && !id.contains("..")
}

fn not_found() -> (String,) {
    ("HTTP/1.0 404 Not Found\r\nContent-Type: text/plain\r\n\r\nArtifact not found".to_string(),)
}

fn http_200(content_type: &str, body: &str) -> (String,) {
    (format!(
        "HTTP/1.0 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ),)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Content embedded inside a `<script>` block as a JS string literal.
/// serde_json handles quoting; escaping `<` blocks `</script>` breakout.
fn js_string(s: &str) -> String {
    serde_json::to_string(s)
        .unwrap_or_else(|_| "\"\"".into())
        .replace('<', "\\u003c")
}

fn serve_artifact(store: &ArtifactStore, id: &str, version: Option<u32>) -> (String,) {
    if !valid_id(id) {
        return not_found();
    }
    let artifact = match version {
        Some(n) => store.get_version(id, n),
        None => store.get(id),
    };
    match artifact {
        Ok(artifact) => {
            let body = render_artifact_page(&artifact, version.is_none(), "");
            http_200("text/html; charset=utf-8", &body)
        }
        Err(_) => not_found(),
    }
}

fn versions_json(store: &ArtifactStore, id: &str) -> (String,) {
    if !valid_id(id) {
        return not_found();
    }
    match store.versions(id) {
        Ok(history) => http_200(
            "application/json",
            &serde_json::to_string_pretty(&history).unwrap_or_else(|_| "[]".into()),
        ),
        Err(_) => not_found(),
    }
}

fn index_page(store: &ArtifactStore) -> (String,) {
    http_200("text/html; charset=utf-8", &render_index(store, ""))
}

/// Render the artifact index (gallery) HTML. `prefix` is the URL path the
/// artifact routes are mounted under ("" for a root-mounted server,
/// "/artifacts" when served from flared) and is baked into every link.
pub fn render_index(store: &ArtifactStore, prefix: &str) -> String {
    let artifacts = store.list(None).unwrap_or_default();
    // Group by session, newest session activity first (list() is already
    // sorted newest-first, and BTreeMap keeps session groups stable).
    let mut sessions: std::collections::BTreeMap<&str, Vec<&crate::types::ArtifactSummary>> =
        std::collections::BTreeMap::new();
    for a in &artifacts {
        sessions.entry(a.session_id.as_str()).or_default().push(a);
    }
    let mut groups = String::new();
    for (session, items) in &sessions {
        let session_label = if session.is_empty() {
            "(no session)"
        } else {
            session
        };
        groups.push_str(&format!("<h2>{}</h2>\n<ul>\n", html_escape(session_label)));
        for a in items {
            let icon = a.favicon.as_deref().unwrap_or("📄");
            let desc = a
                .description
                .as_deref()
                .map(|d| format!("<br><small>{}</small>", html_escape(d)))
                .unwrap_or_default();
            groups.push_str(&format!(
                r#"<li>{} <a href="{prefix}/{}">{}</a> <small>({}, v{}) · <a href="{prefix}/{}/versions">history</a></small>{}</li>"#,
                html_escape(icon),
                a.id,
                html_escape(&a.name),
                a.artifact_type,
                a.version,
                a.id,
                desc
            ));
            groups.push('\n');
        }
        groups.push_str("</ul>\n");
    }
    if groups.is_empty() {
        groups = "<p>No artifacts published yet.</p>".into();
    }
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>agentflare artifacts</title>
<style>body{{font-family:system-ui,sans-serif;max-width:48rem;margin:2rem auto;padding:0 1rem}}ul{{list-style:none;padding:0}}li{{padding:.5rem 0;border-bottom:1px solid #eee}}a{{color:#0066cc;text-decoration:none}}small{{color:#666}}h2{{color:#444;font-size:1rem;margin-top:1.5rem}}</style>
</head>
<body><h1>agentflare artifacts</h1>{groups}</body>
</html>"#
    )
}

/// Shared `<head>` chunk: escaped title, optional emoji favicon, live-reload
/// wiring (only on the latest version — snapshots are immutable).
fn page_head(artifact: &crate::types::Artifact, live: bool, prefix: &str) -> String {
    let mut head = format!(
        "<meta charset=\"utf-8\"><title>{}</title>\n",
        html_escape(&artifact.name)
    );
    if let Some(emoji) = &artifact.favicon {
        head.push_str(&format!(
            "<link rel=\"icon\" href=\"data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'><text y='.9em' font-size='90'>{}</text></svg>\">\n",
            html_escape(emoji)
        ));
    }
    if live {
        head.push_str(&format!(
            "<script>const es=new EventSource('{prefix}/{}/live');es.onmessage=()=>location.reload()</script>\n",
            artifact.id
        ));
    }
    head
}

/// Render a full artifact page. `live` wires up SSE auto-reload (latest
/// version only); `prefix` is the URL path the artifact routes are mounted
/// under ("" for a root-mounted server, "/artifacts" when served from
/// flared) and is baked into the page's internal links.
pub fn render_artifact_page(artifact: &crate::types::Artifact, live: bool, prefix: &str) -> String {
    let head = page_head(artifact, live, prefix);
    let title = html_escape(&artifact.name);
    let snapshot_banner = if live {
        String::new()
    } else {
        format!(
            r#"<p style="background:#fff3cd;padding:.5rem 1rem;border-radius:4px">viewing v{} (read-only snapshot) — <a href="{prefix}/{}">back to latest</a></p>"#,
            artifact.version, artifact.id
        )
    };
    let style = "<style>body{font-family:system-ui,sans-serif;max-width:48rem;margin:2rem auto;padding:0 1rem}pre{background:#f5f5f5;padding:1rem;border-radius:4px;overflow-x:auto}h1{border-bottom:2px solid #eee;padding-bottom:.5rem}</style>";
    match artifact.artifact_type {
        // Html and Diagram (svg) are raw by design: rendering agent-authored
        // documents is the entire point of these types.
        crate::types::ArtifactType::Html => format!(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>{head}</head>\n<body>{snapshot_banner}{}</body>\n</html>",
            artifact.content
        ),
        crate::types::ArtifactType::Diagram => format!(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>{head}{style}</head>\n<body>{snapshot_banner}<h1>{title}</h1>{}</body>\n</html>",
            artifact.content
        ),
        crate::types::ArtifactType::Markdown => format!(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>{head}{style}</head>\n<body>{snapshot_banner}<main id=\"content\"></main>\n<script src=\"https://cdn.jsdelivr.net/npm/marked@12/marked.min.js\"></script>\n<script>document.getElementById('content').innerHTML=marked.parse({src});</script>\n</body>\n</html>",
            src = js_string(&artifact.content)
        ),
        crate::types::ArtifactType::Mermaid => format!(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>{head}{style}</head>\n<body>{snapshot_banner}<h1>{title}</h1><pre class=\"mermaid\">{}</pre>\n<script type=\"module\">import mermaid from 'https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.esm.min.mjs';mermaid.initialize({{startOnLoad:true}});</script>\n</body>\n</html>",
            html_escape(&artifact.content)
        ),
        crate::types::ArtifactType::Text => format!(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>{head}{style}</head>\n<body>{snapshot_banner}<h1>{title}</h1><pre>{}</pre></body>\n</html>",
            html_escape(&artifact.content)
        ),
    }
}

fn serve_sse(stream: &mut TcpStream, store: &ArtifactStore, id: &str) {
    let headers = "HTTP/1.0 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nAccess-Control-Allow-Origin: *\r\n\r\n";
    if stream.write_all(headers.as_bytes()).is_err() {
        return;
    }
    let _ = stream.flush();

    // In-process publishes arrive on the broadcast channel; publishes from
    // OTHER processes (an MCP session while flared serves the store) never
    // reach this process's channel, so poll the disk store as a fallback.
    let rx = store.subscribe(id);
    let mut last = store.get(id).ok().map(|a| (a.version, a.updated_at));
    loop {
        let event = match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(event) => Some(event),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let now = store.get(id).ok().map(|a| (a.version, a.updated_at));
                (now.is_some() && now != last).then(|| "update".to_string())
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        last = store.get(id).ok().map(|a| (a.version, a.updated_at));
        let msg = match event {
            Some(event) => format!("event: {event}\ndata: {event}\n\n"),
            // Comment keepalive: detects dead sockets between updates.
            None => ": keepalive\n\n".to_string(),
        };
        if stream.write_all(msg.as_bytes()).is_err() {
            break;
        }
        let _ = stream.flush();
    }
}

impl Drop for ArtifactServer {
    fn drop(&mut self) {}
}
