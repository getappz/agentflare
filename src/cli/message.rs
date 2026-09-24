//! `agentflare message` -- inter-agent messaging from a shell: humans at a
//! terminal (sender `human:<user>`), or an agent shelling out (sender = its
//! own session, found through its agent process). `watch` streams incoming
//! messages one per line, for Claude Code's Monitor tool or any tail-style
//! consumer.

use crate::mcp_server::AgentflareMcp;
use crate::mcp_server::types::MessageRequest;
use crate::messages::{self, identity};
use clap::{Args, Subcommand};
use std::io::{BufRead, Write};

#[derive(Args)]
pub struct MessageArgs {
    #[command(subcommand)]
    pub command: MessageCommand,
}

#[derive(Subcommand)]
pub enum MessageCommand {
    /// Send a message to a session key/name, item:<id>, agent:<name>, or *.
    Send {
        /// Recipient: a session key or unique name (see `message list`),
        /// item:<id>, agent:<name>, or * for every live session.
        to: String,
        /// Message text (joined with spaces).
        #[arg(required = true, num_args = 1..)]
        body: Vec<String>,
        /// Id of the message this answers.
        #[arg(long)]
        reply_to: Option<i64>,
    },
    /// List live agent sessions.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show your messages (unread by default).
    Inbox {
        /// Include messages already marked read.
        #[arg(long)]
        all: bool,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Mark messages read.
    Read {
        #[arg(required = true, num_args = 1..)]
        ids: Vec<i64>,
    },
    /// Print the session key you send and receive as.
    Whoami,
    /// Stream incoming messages, one line each, until interrupted.
    Watch {
        /// Mailbox to watch (default: your own).
        #[arg(long)]
        to: Option<String>,
        /// Observe without taking: messages stay undelivered for the
        /// recipient's own hooks (default when watching someone else's
        /// mailbox; watching your own takes them).
        #[arg(long)]
        peek: bool,
        /// One JSON object per line instead of text.
        #[arg(long)]
        json: bool,
        /// Dashboard/daemon port to stream from; falls back to polling the
        /// db every second when nothing listens there.
        #[arg(long, default_value_t = crate::mcp_server::types::FLARED_DEFAULT_PORT)]
        port: u16,
    },
}

fn fail(e: impl std::fmt::Display) -> ! {
    crate::ui::error(&e.to_string());
    std::process::exit(1);
}

fn open() -> rusqlite::Connection {
    crate::db::open().unwrap_or_else(|e| fail(format!("opening agentflare.db: {e}")))
}

impl MessageArgs {
    pub fn run(self) {
        let conn = open();
        let me = identity::cli_key(&conn);
        let run = |req: MessageRequest| -> serde_json::Value {
            let out = AgentflareMcp::default()
                .message_as(&conn, &me, req)
                .unwrap_or_else(|e| fail(e.message));
            serde_json::from_str(&out).unwrap_or_default()
        };
        match self.command {
            MessageCommand::Send { to, body, reply_to } => {
                let v = run(MessageRequest {
                    action: "send".into(),
                    to: Some(to),
                    body: Some(body.join(" ")),
                    reply_to,
                    ..Default::default()
                });
                let recipients: Vec<&str> = v["recipients"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|r| r.as_str()).collect())
                    .unwrap_or_default();
                if recipients.is_empty() {
                    println!("No live session on that item; left as an item comment.");
                } else {
                    println!("Sent as {me} to {}", recipients.join(", "));
                }
            }
            MessageCommand::List { json } => {
                let v = run(MessageRequest {
                    action: "list".into(),
                    ..Default::default()
                });
                if json {
                    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                    return;
                }
                let sessions = v["sessions"].as_array().cloned().unwrap_or_default();
                if sessions.is_empty() {
                    println!("No live agent sessions.");
                }
                for s in sessions {
                    let str_of = |k: &str| s[k].as_str().unwrap_or("-").to_string();
                    println!(
                        "{}{}  name={}  item={}  idle={}s  cwd={}",
                        if s["you"].as_bool() == Some(true) {
                            "* "
                        } else {
                            "  "
                        },
                        str_of("key"),
                        str_of("name"),
                        str_of("item"),
                        s["idle_secs"].as_i64().unwrap_or(0),
                        str_of("cwd"),
                    );
                }
            }
            MessageCommand::Inbox { all, limit, json } => {
                let v = run(MessageRequest {
                    action: "inbox".into(),
                    unread_only: Some(!all),
                    limit: Some(limit),
                    ..Default::default()
                });
                if json {
                    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                    return;
                }
                let msgs: Vec<messages::Message> =
                    serde_json::from_value(v["messages"].clone()).unwrap_or_default();
                if msgs.is_empty() {
                    println!("No messages for {me}.");
                }
                for m in msgs.iter().rev() {
                    println!("{}", messages::format_line(m));
                }
            }
            MessageCommand::Read { ids } => {
                let v = run(MessageRequest {
                    action: "read".into(),
                    ids: Some(ids),
                    ..Default::default()
                });
                println!("Marked {} read.", v["marked_read"].as_i64().unwrap_or(0));
            }
            MessageCommand::Whoami => println!("{me}"),
            MessageCommand::Watch {
                to,
                peek,
                json,
                port,
            } => {
                let take = to.is_none() && !peek;
                let to = to.unwrap_or(me);
                drop(conn);
                watch(&to, take, json, port);
            }
        }
    }
}

fn emit(m: &messages::Message, json: bool) {
    let line = if json {
        serde_json::to_string(m).unwrap_or_default()
    } else {
        messages::format_line(m)
    };
    let mut out = std::io::stdout().lock();
    if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
        // The consumer went away (closed pipe): nothing left to do.
        std::process::exit(0);
    }
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Streams `to`'s messages: over the daemon's SSE endpoint when it's up
/// (pushed the moment they're sent), else by polling the db every second.
/// With `take`, each message is marked delivered as it's printed.
fn watch(to: &str, take: bool, json: bool, port: u16) {
    let mut after = open_after_cursor(take);
    loop {
        if let Some(last) = watch_sse(to, take, json, port, after) {
            after = after.max(last);
        }
        // Daemon unreachable (or the stream ended): poll until it's back.
        let conn = open();
        let mut ticks = 0u32;
        loop {
            let now = crate::claims::now();
            let batch = if take {
                messages::take_undelivered(&conn, to, messages::MAX_BATCH, now)
            } else {
                messages::since(&conn, Some(to), after, messages::MAX_BATCH)
            }
            .unwrap_or_default();
            for m in &batch {
                after = after.max(m.id);
                emit(m, json);
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
            ticks += 1;
            // Retry the push stream now and then.
            if ticks.is_multiple_of(30) {
                break;
            }
        }
    }
}

/// Where an observe-only watch starts: only messages sent from now on.
fn open_after_cursor(take: bool) -> i64 {
    if take {
        return 0;
    }
    messages::max_id(&open()).unwrap_or(0)
}

/// Reads the SSE stream until it ends; returns the last id seen, or `None`
/// if it couldn't connect.
fn watch_sse(to: &str, take: bool, json: bool, port: u16, after: i64) -> Option<i64> {
    let url = format!(
        "http://127.0.0.1:{port}/api/messages/stream?to={}&take={take}&after={after}",
        encode(to)
    );
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_millis(500))
        .build();
    let resp = agent.get(&url).call().ok()?;
    let mut last = after;
    for line in std::io::BufReader::new(resp.into_reader()).lines() {
        let Ok(line) = line else { break };
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        if let Ok(m) = serde_json::from_str::<messages::Message>(data.trim()) {
            last = last.max(m.id);
            emit(&m, json);
        }
    }
    Some(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_keeps_keys_readable_and_escapes_the_rest() {
        assert_eq!(encode("claude-code:abc-1"), "claude-code:abc-1");
        assert_eq!(encode("*"), "%2A");
        assert_eq!(encode("a b&c"), "a%20b%26c");
    }
}
