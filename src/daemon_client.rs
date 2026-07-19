// Scaffolding for the daemon HTTP server / foreground-daemon wiring,
// landing in a follow-up PR (see task-report.md). Not reachable yet.
#![allow(dead_code)]

use crate::daemon::{is_daemon_running, start_daemon};
use crate::ipc::{DaemonAddr, connect};

pub fn daemon_request(
    addr: &DaemonAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<String, String> {
    let stream = connect(addr)?;
    let body_bytes = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body_bytes}",
        body_bytes.len()
    );
    stream.write_all(request.as_bytes())?;
    let response = stream.read_all()?;
    let text = String::from_utf8(response).map_err(|e| format!("utf-8: {e}"))?;

    let mut lines = text.lines();
    let status_line = lines.next().unwrap_or("");
    let parts: Vec<&str> = status_line.splitn(3, ' ').collect();
    if parts.len() < 2 {
        return Err("malformed HTTP response".to_string());
    }
    let status_code: u16 = parts[1].parse().unwrap_or(0);

    let mut body_start = false;
    let mut response_body = String::new();
    for line in lines {
        if body_start {
            if !response_body.is_empty() {
                response_body.push('\n');
            }
            response_body.push_str(line);
        } else if line.is_empty() {
            body_start = true;
        }
    }

    if (200..300).contains(&status_code) {
        Ok(response_body)
    } else {
        Err(format!("HTTP {status_code}: {response_body}"))
    }
}

pub fn daemon_health_check(addr: &DaemonAddr) -> Result<String, String> {
    daemon_request(addr, "GET", "/health", None)
}

pub fn daemon_tool_call(addr: &DaemonAddr, tool: &str, args: &str) -> Result<String, String> {
    let body = format!(r#"{{"tool":"{tool}","args":{args}}}"#);
    daemon_request(addr, "POST", "/v1/tools/call", Some(&body))
}

pub fn try_daemon_tool_call_blocking(tool: &str, args: &str) -> Result<String, String> {
    if let Some(pid) = is_daemon_running() {
        let addr = DaemonAddr::default_for_pid(pid);
        return daemon_tool_call(&addr, tool, args);
    }

    let pid = start_daemon()?;
    let addr = DaemonAddr::default_for_pid(pid);

    for _ in 0..10 {
        if daemon_health_check(&addr).is_ok() {
            return daemon_tool_call(&addr, tool, args);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    Err("daemon did not become healthy".to_string())
}
