//! Test-only HTTP mock for exercising the GitHub client and resource
//! functions without touching the network. A `MockServer` binds to an
//! ephemeral localhost port, answers requests, and records what it received
//! so tests can assert on method, path, and body.
//!
//! Two flavors: `start` replays a fixed queue of canned responses, and
//! `start_with` answers from a handler closure. The handler form exists for
//! tests where the server must model STATE rather than a script — an issue's
//! comment list that two bridge instances both read from and write to cannot
//! be expressed as a fixed sequence, because what the second instance sees
//! depends on what the first one did.

use super::Client;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

/// One canned response the mock server will hand out, in order.
pub struct MockResponse {
    pub status: u16,
    pub body: String,
    pub headers: Vec<(String, String)>,
}

impl MockResponse {
    pub fn json(status: u16, body: &str) -> Self {
        MockResponse {
            status,
            body: body.to_string(),
            headers: Vec::new(),
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// A request the mock server observed.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub body: String,
}

pub struct MockServer {
    pub base_url: String,
    handle: Option<JoinHandle<Vec<RecordedRequest>>>,
    stop: Arc<AtomicBool>,
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        _ => "Status",
    }
}

impl MockServer {
    /// Start a server that hands out `responses` in order.
    ///
    /// An extra request past the end of the queue gets a 500 naming the
    /// offending method and path. That is deliberately a loud failure rather
    /// than the silent one it replaced: with nothing left to write, the
    /// server used to leave the connection open and the client blocked on a
    /// read that never came, so a test that under-queued by one response hung
    /// until the harness timed it out instead of saying what was missing.
    pub fn start(responses: Vec<MockResponse>) -> MockServer {
        let queue = Mutex::new(responses.into_iter());
        MockServer::start_with(move |req| {
            queue.lock().unwrap().next().unwrap_or_else(|| {
                MockResponse::json(
                    500,
                    &format!(
                        r#"{{"message":"mock server: no canned response left for {} {}"}}"#,
                        req.method, req.path
                    ),
                )
            })
        })
    }

    /// Start a server that answers each request from `handler`.
    ///
    /// Unlike `start`, this serves an unbounded number of requests and lets
    /// the handler close over shared mutable state, so a test can model a
    /// resource that changes as clients act on it.
    pub fn start_with<F>(handler: F) -> MockServer
    where
        F: Fn(&RecordedRequest) -> MockResponse + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let port = listener.local_addr().unwrap().port();
        let base_url = format!("http://127.0.0.1:{port}");
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);

        let handle = std::thread::spawn(move || {
            let mut recorded = Vec::new();
            loop {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                // `requests()` wakes this blocking accept with a throwaway
                // connection; that one carries no request to answer.
                if thread_stop.load(Ordering::SeqCst) {
                    break;
                }
                if let Some(req) = handle_connection(&mut stream) {
                    let response = handler(&req);
                    write_response(&mut stream, &response);
                    recorded.push(req);
                }
            }
            recorded
        });

        MockServer {
            base_url,
            handle: Some(handle),
            stop,
        }
    }

    /// A client pointed at this mock server. `token` controls whether the
    /// client is authenticated (writes require a token).
    pub fn client(&self, token: Option<&str>) -> Client {
        Client::for_test(self.base_url.clone(), token.map(str::to_string))
    }

    /// Stop the server and return every request it saw, in order.
    pub fn requests(mut self) -> Vec<RecordedRequest> {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock the accept: the loop only re-checks `stop` once a
        // connection arrives, and no client is going to send another.
        let _ = std::net::TcpStream::connect(
            self.base_url
                .strip_prefix("http://")
                .unwrap_or(&self.base_url),
        );
        self.handle
            .take()
            .expect("server already joined")
            .join()
            .expect("mock server thread panicked")
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        // A server the test never called `requests()` on would otherwise
        // leave its thread parked in `accept` for the life of the test
        // binary. Signal and wake it; don't join, since a drop must not block.
        if self.handle.is_some() {
            self.stop.store(true, Ordering::SeqCst);
            let _ = std::net::TcpStream::connect(
                self.base_url
                    .strip_prefix("http://")
                    .unwrap_or(&self.base_url),
            );
        }
    }
}

fn handle_connection(stream: &mut std::net::TcpStream) -> Option<RecordedRequest> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);

    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).ok()?;
    }

    Some(RecordedRequest {
        method,
        path,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn write_response(stream: &mut std::net::TcpStream, response: &MockResponse) {
    let mut out = format!(
        "HTTP/1.1 {} {}\r\n",
        response.status,
        reason(response.status)
    );
    out.push_str("Content-Type: application/json\r\n");
    out.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    for (name, value) in &response.headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("Connection: close\r\n\r\n");
    out.push_str(&response.body);
    let _ = stream.write_all(out.as_bytes());
    let _ = stream.flush();
}
