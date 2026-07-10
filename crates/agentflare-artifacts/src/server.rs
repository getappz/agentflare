use crate::store::ArtifactStore;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

pub struct ArtifactServer {
    port: u16,
    store: Arc<ArtifactStore>,
}

impl ArtifactServer {
    pub fn start(store: Arc<ArtifactStore>) -> std::io::Result<Self> {
        let port = find_available_port(0);
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let actual_port = listener.local_addr()?.port();
        let server = ArtifactServer {
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
        format!("http://127.0.0.1:{}/{id}", self.port)
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

fn find_available_port(preferred: u16) -> u16 {
    if preferred == 0 {
        TcpListener::bind("127.0.0.1:0")
            .ok()
            .and_then(|l| l.local_addr().ok())
            .map(|a| a.port())
            .unwrap_or(18789)
    } else {
        TcpListener::bind(("127.0.0.1", preferred))
            .ok()
            .and_then(|l| l.local_addr().ok())
            .map(|a| a.port())
            .unwrap_or_else(|| find_available_port(0))
    }
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

        (method.to_string(), path.to_string(), content_length, reader.into_inner())
    };

    let (method, path, _content_length, _remaining) = buf;

    let response = match (method.as_str(), path.as_str()) {
        ("GET", path) if path == "/" => index_page(store),
        ("GET", path) if path.ends_with("/live") => {
            let id = path.strip_suffix("/live").unwrap_or("").trim_start_matches('/');
            return serve_sse(&mut stream, store, id);
        }
        ("GET", path) => {
            let id = path.trim_start_matches('/');
            serve_artifact(store, id)
        }
        _ => (
            "HTTP/1.0 405 Method Not Allowed\r\n\r\nMethod Not Allowed".to_string(),
        ),
    };

    let _ = stream.write_all(response.0.as_bytes());
    let _ = stream.flush();
}

fn serve_artifact(store: &ArtifactStore, id: &str) -> (String,) {
    match store.get(id) {
        Ok(artifact) => {
            let body = render_artifact_page(&artifact);
            let headers = format!(
                "HTTP/1.0 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
                body.len()
            );
            (format!("{headers}{body}"),)
        }
        Err(_) => (
            "HTTP/1.0 404 Not Found\r\nContent-Type: text/plain\r\n\r\nArtifact not found"
                .to_string(),
        ),
    }
}

fn index_page(store: &ArtifactStore) -> (String,) {
    let artifacts = store.list(None).unwrap_or_default();
    let mut items = String::new();
    for a in &artifacts {
        items.push_str(&format!(
            r#"<li><a href="/{}">{}</a> <small>({})</small></li>"#,
            a.id, a.name, a.artifact_type
        ));
    }
    let body = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>agentflare artifacts</title>
<style>body{{font-family:system-ui,sans-serif;max-width:48rem;margin:2rem auto;padding:0 1rem}}ul{{list-style:none;padding:0}}li{{padding:.5rem 0;border-bottom:1px solid #eee}}a{{color:#0066cc;text-decoration:none}}small{{color:#666}}</style>
</head>
<body><h1>agentflare artifacts</h1><ul>{items}</ul></body>
</html>"#
    );
    let headers = format!(
        "HTTP/1.0 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    (format!("{headers}{body}"),)
}

fn render_artifact_page(artifact: &crate::types::Artifact) -> String {
    let live_url = format!("/{}/live", artifact.id);
    match artifact.artifact_type {
        crate::types::ArtifactType::Html => {
            format!(
                r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>{title}</title>
<script>const es=new EventSource('{live}');es.onmessage=()=>location.reload()</script>
</head>
<body>{content}</body>
</html>"#,
                title = artifact.name,
                live = live_url,
                content = artifact.content
            )
        }
        _ => {
            format!(
                r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>{title}</title>
<script>const es=new EventSource('{live}');es.onmessage=()=>location.reload()</script>
<style>body{{font-family:system-ui,sans-serif;max-width:48rem;margin:2rem auto;padding:0 1rem}}pre{{background:#f5f5f5;padding:1rem;border-radius:4px;overflow-x:auto}}h1{{border-bottom:2px solid #eee;padding-bottom:.5rem}}</style>
</head>
<body><h1>{title}</h1><pre>{content}</pre></body>
</html>"#,
                title = artifact.name,
                live = live_url,
                content = artifact.content
            )
        }
    }
}

fn serve_sse(stream: &mut TcpStream, store: &ArtifactStore, id: &str) {
    let headers = "HTTP/1.0 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nAccess-Control-Allow-Origin: *\r\n\r\n";
    if stream.write_all(headers.as_bytes()).is_err() {
        return;
    }
    let _ = stream.flush();

    let rx = store.subscribe(id);
    while let Ok(event) = rx.recv() {
        let msg = format!("event: {event}\ndata: {event}\n\n");
        if stream.write_all(msg.as_bytes()).is_err() {
            break;
        }
        let _ = stream.flush();
    }
}

impl Drop for ArtifactServer {
    fn drop(&mut self) {}
}
