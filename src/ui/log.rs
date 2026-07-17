//! Status output — one styled line each. On a terminal these render as cliclack
//! log lines; off a terminal they degrade to plain prefixed text so piped
//! output and CI logs stay readable.

use super::interactive;

/// Wizard header.
pub fn intro(message: &str) {
    if interactive() {
        let _ = cliclack::intro(message);
    } else {
        println!("{message}");
    }
}

/// Wizard footer.
pub fn outro(message: &str) {
    if interactive() {
        let _ = cliclack::outro(message);
    } else {
        println!("{message}");
    }
}

/// Completed action (green check on a terminal).
pub fn success(message: &str) {
    if interactive() {
        let _ = cliclack::log::success(message);
    } else {
        println!("ok    {message}");
    }
}

/// Failure. Off a terminal it goes to stderr, where failures belong.
pub fn error(message: &str) {
    if interactive() {
        let _ = cliclack::log::error(message);
    } else {
        eprintln!("fail  {message}");
    }
}

/// Caution the user should notice but that isn't fatal.
pub fn warning(message: &str) {
    if interactive() {
        let _ = cliclack::log::warning(message);
    } else {
        println!("warn  {message}");
    }
}

/// Neutral informational line.
pub fn info(message: &str) {
    if interactive() {
        let _ = cliclack::log::info(message);
    } else {
        println!("info  {message}");
    }
}

/// An in-progress step in a sequence (a wired component, a written file).
pub fn step(message: &str) {
    if interactive() {
        let _ = cliclack::log::step(message);
    } else {
        println!("      {message}");
    }
}

/// "Nothing to do" — dimmer than [`info`], for already-satisfied work.
pub fn skip(message: &str) {
    if interactive() {
        let _ = cliclack::log::remark(message);
    } else {
        println!("skip  {message}");
    }
}
