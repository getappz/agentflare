//! What the capture reader does with a run's stdout as it arrives: parse a
//! CLI's structured stream into a [`TurnState`], or speak ACP with the agent
//! ([`AcpCore`]), writing its replies to the child's stdin. Either way the
//! result is one [`TurnState`] and, once its terminal event lands, the
//! instant that may end the run.

use super::acp::{AcpCore, Approver, Out};
use super::turn::TurnState;
use agent_registry::{ReplyFormat, Structured};
use std::io::Write;
use std::process::ChildStdin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

/// How a captured run's stdout is read.
pub(crate) enum Stream {
    /// A CLI's structured output.
    Cli(Structured),
    /// An ACP server, answered over stdin; permission requests go to the
    /// approver.
    Acp(Box<AcpCore>, Approver),
}

impl Stream {
    /// The child's stdin must stay open for the stream's own writes.
    pub(crate) fn writes_stdin(&self) -> bool {
        matches!(self, Self::Acp(..))
    }
}

/// The turn read so far, shared by the reader, the wait loop and any
/// in-flight ACP permission decision.
pub(crate) type SharedLive = Arc<Mutex<Option<Live>>>;

pub(crate) struct Live {
    pub turn: TurnState,
    /// When the terminal event arrived, for a run its terminal event may
    /// end (`Structured::ends_at_result`; always for ACP, whose server
    /// never exits on its own).
    pub ended_at: Option<Instant>,
    ends_at_result: bool,
    acp: Option<AcpLink>,
}

struct AcpLink {
    core: AcpCore,
    stdin: Option<ChildStdin>,
    approver: Approver,
}

impl Live {
    pub(crate) fn new(stream: Stream, stdin: Option<ChildStdin>) -> Self {
        let (format, ends_at_result, acp) = match stream {
            Stream::Cli(s) => (s.format, s.ends_at_result, None),
            Stream::Acp(core, approver) => (
                ReplyFormat::AcpUpdates,
                true,
                Some(AcpLink {
                    core: *core,
                    stdin,
                    approver,
                }),
            ),
        };
        Self {
            turn: TurnState::new(format),
            ended_at: None,
            ends_at_result,
            acp,
        }
    }

    /// Carries out the core's requests: lines go to the agent; a decision
    /// runs on its own thread so the reader keeps reading meanwhile.
    fn apply(&mut self, outs: Vec<Out>, shared: &SharedLive) {
        let Some(acp) = self.acp.as_mut() else {
            return;
        };
        for out in outs {
            match out {
                Out::Send(line) => {
                    let written = acp
                        .stdin
                        .as_mut()
                        .is_some_and(|s| writeln!(s, "{line}").and_then(|()| s.flush()).is_ok());
                    if !written {
                        acp.stdin = None;
                    }
                }
                Out::Decide { ask, tool, input } => {
                    let (approver, shared) = (acp.approver.clone(), shared.clone());
                    // fire-and-forget: the answer reaches the agent through
                    // `shared`; a run killed first simply drops it.
                    std::thread::spawn(move || {
                        let allow = approver(&tool, &input);
                        let mut guard = lock(&shared);
                        if let Some(live) = guard.as_mut() {
                            let outs = live
                                .acp
                                .as_mut()
                                .map(|a| a.core.answer(ask, allow))
                                .unwrap_or_default();
                            live.apply(outs, &shared);
                        }
                    });
                }
            }
        }
    }
}

fn lock(shared: &SharedLive) -> MutexGuard<'_, Option<Live>> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Sends what an ACP agent needs first (`initialize`); nothing for a CLI.
pub(crate) fn start(shared: &SharedLive) {
    let mut guard = lock(shared);
    if let Some(live) = guard.as_mut() {
        let outs = live
            .acp
            .as_mut()
            .map(|a| a.core.start())
            .unwrap_or_default();
        live.apply(outs, shared);
    }
}

/// Feeds the stdout read so far from `*consumed` (see `TurnState::feed`),
/// answering an ACP agent line by line.
pub(crate) fn feed(shared: &SharedLive, buf: &[u8], consumed: &mut usize) {
    let mut guard = lock(shared);
    let Some(live) = guard.as_mut() else {
        return;
    };
    match live.acp.as_mut() {
        None => live.turn.feed(buf, consumed),
        Some(acp) => {
            let mut outs = Vec::new();
            while let Some(newline) = buf[*consumed..].iter().position(|b| *b == b'\n') {
                let end = *consumed + newline + 1;
                outs.extend(acp.core.line(&buf[*consumed..end], &mut live.turn));
                *consumed = end;
            }
            live.apply(outs, shared);
        }
    }
    if live.ends_at_result && live.turn.terminal && live.ended_at.is_none() {
        live.ended_at = Some(Instant::now());
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
