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
