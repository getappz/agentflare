//! Spinner for long-running work. Off a terminal it prints plain start/done
//! lines instead of an animated spinner, so nothing depends on ANSI control
//! codes reaching a pipe.

use super::interactive;

/// Run `work` behind a spinner labelled `start`, replacing it with `done` on
/// completion. Returns whatever `work` returns.
pub fn with_spinner<T>(start: &str, done: &str, work: impl FnOnce() -> T) -> T {
    if !interactive() {
        println!("{start}");
        let out = work();
        println!("{done}");
        return out;
    }
    let sp = cliclack::spinner();
    sp.start(start);
    let out = work();
    sp.stop(done);
    out
}

/// Determinate progress over a known number of steps. Off a terminal each
/// step prints one plain line instead of redrawing a bar.
pub struct Progress(Option<cliclack::ProgressBar>);

impl Progress {
    pub fn start(len: u64, message: &str) -> Progress {
        if !interactive() {
            println!("{message}");
            return Progress(None);
        }
        let bar = cliclack::progress_bar(len);
        bar.start(message);
        Progress(Some(bar))
    }

    /// One step finished; `message` names it.
    pub fn inc(&self, message: &str) {
        match &self.0 {
            Some(bar) => {
                bar.set_message(message);
                bar.inc(1);
            }
            None => println!("      {message}"),
        }
    }

    pub fn stop(self, message: &str) {
        match self.0 {
            Some(bar) => bar.stop(message),
            None => println!("{message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_usable_off_a_terminal() {
        let p = Progress::start(2, "Removing");
        p.inc("one");
        p.inc("two");
        p.stop("done");
    }
}
