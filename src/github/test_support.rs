//! Test-only HTTP mock for exercising the GitHub client and resource
//! functions without touching the network. A `MockServer` binds to an
//! ephemeral localhost port, serves a fixed queue of canned responses, and
//! records the requests it received so tests can assert on method, path, and
//! body.

use super::Client;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
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
    /// Start a server that will serve exactly `responses.len()` requests, one
    /// per queued response, then stop.
    pub fn start(responses: Vec<MockResponse>) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let port = listener.local_addr().unwrap().port();
        let base_url = format!("http://127.0.0.1:{port}");

        let handle = std::thread::spawn(move || {
            let mut recorded = Vec::new();
            for response in responses {
                let (stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(_) => break,
                };
                let mut stream = stream;
                let recorded_req = handle_connection(&mut stream);
                write_response(&mut stream, &response);
                if let Some(req) = recorded_req {
                    recorded.push(req);
                }
            }
            recorded
        });

        MockServer {
            base_url,
            handle: Some(handle),
        }
    }

    /// A client pointed at this mock server. `token` controls whether the
    /// client is authenticated (writes require a token).
    pub fn client(&self, token: Option<&str>) -> Client {
        Client::for_test(self.base_url.clone(), token.map(str::to_string))
    }

    /// Stop the server and return every request it saw, in order.
    pub fn requests(mut self) -> Vec<RecordedRequest> {
        self.handle
            .take()
            .expect("server already joined")
            .join()
            .expect("mock server thread panicked")
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
