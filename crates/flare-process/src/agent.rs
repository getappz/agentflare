//! Caller-agent detection: which AI coding agent, if any, is driving the
//! current process.
//!
//! Two signals, checked in this order:
//! 1. An ancestor process whose executable name is a known agent CLI
//!    (`claude`, `opencode`, `codex`, ...). Survives env-var stripping, so
//!    it is the signal the git PATH shim's agent-only policy leans on.
//! 2. Agent-specific environment markers, via the `agent-detector` crate
//!    (compiled WITHOUT its `process-tree` feature -- see below).
//!
//! Why the ancestor walk is implemented here instead of using
//! `agent-detector`'s own: its walk calls
//! `sysinfo::System::refresh_processes(ProcessesToUpdate::All)`, which on
//! Windows takes a Toolhelp snapshot and then `OpenProcess`es + queries
//! EVERY process on the machine just to read a handful of parent links.
//! Under a parallel `cargo nextest` run on windows-latest that call
//! intermittently blocks for good -- tests that did nothing but call
//! `agent_detector::agent_name()` sat at 100% of nextest's 300s
//! slow-timeout and passed instantly on retry (item #314, six windows CI
//! runs 2026-09-23..25; the same walk runs inside every `git` invocation
//! that resolves to the shim, which is how `throwaway_repo()`-style
//! fixtures hung too). The chain of ancestors is all this needs, so this
//! walk reads the snapshot's own pid/ppid/name table (no `OpenProcess` at
//! all) on Windows and refreshes one pid per hop through sysinfo elsewhere.
//!
//! The ancestor half is computed once per process: a process's ancestry
//! cannot change during its lifetime. The env-var half is re-read on every
//! call (it is cheap, and tests toggle those markers at runtime).

use std::sync::OnceLock;

/// Upper bound on ancestor hops. Real chains are well under ten deep; the
/// cap is a safety net against pid reuse producing an apparent cycle.
const MAX_HOPS: usize = 32;

/// `(agent name, executable names)` -- the agent-detector 0.2.1 process
/// catalog (`src/agents.rs`, `process_names`), entries with no process
/// name omitted. Names match `agent_detector::agent_name()`'s output so
/// callers see one identity regardless of which signal fired. The crate
/// keeps its catalog private, hence the copy; bump it alongside the
/// `agent-detector` version in this crate's `Cargo.toml`.
const PROCESS_NAMES: &[(&str, &[&str])] = &[
    ("cursor", &["cursor"]),
    ("gemini", &["gemini"]),
    ("codex", &["codex"]),
    ("antigravity", &["amp"]),
    ("augment-cli", &["augment-cli"]),
    ("opencode", &["opencode"]),
    ("claude-code", &["claude"]),
    ("goose", &["goose"]),
    ("trae", &["trae"]),
    ("github-copilot", &["copilot"]),
    ("aider", &["aider"]),
    ("carapace", &["cara"]),
    ("codebuddy", &["codebuddy"]),
    ("devin", &["devin"]),
    ("gloamy", &["gloamy"]),
    ("hermes", &["hermes"]),
    ("ironclaw", &["ironclaw"]),
    ("kimi-cli", &["kimi", "kimi-cli"]),
    ("loong", &["loong"]),
    ("microclaw", &["microclaw"]),
    ("moltis", &["moltis"]),
    ("nanobot", &["nanobot"]),
    ("picoclaw", &["picoclaw"]),
    ("windsurf", &["windsurf"]),
    ("zeroclaw", &["zeroclaw"]),
    ("alayacore", &["alayacore"]),
    ("anda-bot", &["anda"]),
    ("astrbot", &["astrbot"]),
    ("autohand-code", &["autohand"]),
    ("axiomate", &["axiomate"]),
    ("clawx", &["clawx"]),
    ("codeproxy-cli", &["codeproxy"]),
    ("cow-agent", &["cow"]),
    ("crush", &["crush"]),
    ("ctrl", &["ctrl"]),
    ("deep-code", &["deepcode"]),
    ("deep-copilot", &["deep-copilot"]),
    ("deeplossless", &["deeplossless"]),
    ("deepseek-tui", &["deepseek-tui"]),
    ("deepseekx", &["deepseekx"]),
    ("dscli", &["dscli"]),
    ("dscode", &["dscode"]),
    ("goagent", &["goagent"]),
    ("halfcopilot", &["halfcopilot"]),
    ("kilo-code", &["kilo"]),
    ("kimix", &["kimix"]),
    ("langbot", &["langbot"]),
    ("langcli", &["langcli"]),
    ("markus", &["markus"]),
    ("morph", &["mistermorph"]),
    ("oh-my-pi", &["omp"]),
    ("operit", &["operit"]),
    ("proma", &["proma"]),
    ("qwen-code", &["qwen"]),
    ("reasonix", &["reasonix"]),
    ("snow-cli", &["snow"]),
    ("soloncode", &["soloncode"]),
    ("tday", &["tday"]),
    ("tiangong", &["tiangong"]),
    ("whale", &["whale"]),
    ("xpro", &["xpro"]),
    ("zot", &["zot"]),
];

/// The detected agent's name (lowercase, e.g. `"claude-code"`), or `None`
/// when no ancestor process and no env marker identifies one. Drop-in for
/// `agent_detector::agent_name()`, same names, same tier order.
#[must_use]
pub fn agent_name() -> Option<String> {
    ancestor_agent().clone().or_else(agent_detector::agent_name)
}

/// `true` if [`agent_name`] finds an agent.
#[must_use]
pub fn is_agent() -> bool {
    agent_name().is_some()
}

fn ancestor_agent() -> &'static Option<String> {
    static ANCESTOR: OnceLock<Option<String>> = OnceLock::new();
    ANCESTOR.get_or_init(|| agent_from_names(ancestor_names()))
}

/// First ancestor (nearest first) whose executable name is in the catalog.
fn agent_from_names(names: impl IntoIterator<Item = String>) -> Option<String> {
    for name in names {
        for (agent, candidates) in PROCESS_NAMES {
            if candidates.iter().any(|c| process_name_matches(&name, c)) {
                // Same override agent-detector applies: Claude Code running
                // as Cowork reports itself under that name.
                if *agent == "claude-code"
                    && std::env::var("CLAUDE_CODE_IS_COWORK").is_ok_and(|v| !v.trim().is_empty())
                {
                    return Some("cowork".to_string());
                }
                return Some((*agent).to_string());
            }
        }
    }
    None
}

/// `claude.exe`/`Claude` both match `claude`; mirrors agent-detector's
/// `is_process_match`.
fn process_name_matches(name: &str, candidate: &str) -> bool {
    name.strip_suffix(".exe")
        .unwrap_or(name)
        .eq_ignore_ascii_case(candidate)
}

/// Executable names of this process's ancestors, parent first. Best-effort:
/// an unreadable link ends the walk early rather than failing.
#[cfg(windows)]
fn ancestor_names() -> Vec<String> {
    use std::collections::HashMap;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    // pid -> (parent pid, exe name). One snapshot, read once; no per-process
    // handle is ever opened (the whole point, see the module doc).
    let mut table: HashMap<u32, (u32, String)> = HashMap::new();
    // SAFETY: plain Win32 Toolhelp usage -- `entry` is zeroed with `dwSize`
    // set as the API requires, the snapshot handle is checked against
    // INVALID_HANDLE_VALUE before use and closed exactly once.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Vec::new();
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                table.insert(entry.th32ProcessID, (entry.th32ParentProcessID, name));
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }

    let mut names = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut pid = std::process::id();
    while names.len() < MAX_HOPS && seen.insert(pid) {
        let Some((parent, _)) = table.get(&pid) else {
            break;
        };
        let Some((_, parent_name)) = table.get(parent) else {
            break;
        };
        names.push(parent_name.clone());
        pid = *parent;
    }
    names
}

/// Executable names of this process's ancestors, parent first. Refreshes
/// one pid per hop (`ProcessesToUpdate::Some`), never the whole table.
#[cfg(unix)]
fn ancestor_names() -> Vec<String> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let mut sys = System::new();
    let mut names = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let me = Pid::from_u32(std::process::id());
    let mut pid = me;
    while names.len() < MAX_HOPS && seen.insert(pid) {
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            false,
            ProcessRefreshKind::nothing(),
        );
        let Some(proc_) = sys.process(pid) else {
            break;
        };
        if pid != me {
            names.push(proc_.name().to_string_lossy().into_owned());
        }
        let Some(parent) = proc_.parent() else {
            break;
        };
        pid = parent;
    }
    names
}

/// No process-table access on other targets: only the env-marker tier can
/// identify an agent there.
#[cfg(not(any(unix, windows)))]
fn ancestor_names() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_name_matching_strips_exe_and_ignores_case() {
        assert!(process_name_matches("claude", "claude"));
        assert!(process_name_matches("Claude.exe", "claude"));
        assert!(process_name_matches("CLAUDE", "claude"));
        assert!(!process_name_matches("cursor", "claude"));
        assert!(!process_name_matches("codex", "codex.exe"));
    }

    #[test]
    fn nearest_matching_ancestor_wins_and_unknown_names_are_skipped() {
        let names = ["cargo-nextest.exe", "opencode", "claude"].map(String::from);
        assert_eq!(agent_from_names(names).as_deref(), Some("opencode"));
        assert_eq!(
            agent_from_names(["bash", "Runner.Worker.exe"].map(String::from)),
            None
        );
        assert_eq!(agent_from_names(Vec::new()), None);
    }

    #[test]
    fn ancestor_walk_terminates_without_a_shell_or_handles_left_open() {
        // The chain is bounded and every name is a bare executable name, never
        // a path -- a regression here would mean the walk resolved something
        // other than the snapshot's own exe field.
        let names = ancestor_names();
        assert!(names.len() <= MAX_HOPS);
        for name in &names {
            assert!(
                !name.contains('/') && !name.contains('\\'),
                "ancestor name must be a bare executable name, got {name:?}"
            );
        }
        // Cached: a second call must be the identical value.
        assert_eq!(ancestor_agent(), ancestor_agent());
    }

    #[test]
    fn env_marker_tier_still_reaches_agent_detector() {
        // Only the "detects" direction: under a real agent session an
        // ancestor may already match, so the exact name is environment-
        // dependent, but a CLAUDECODE marker must never yield `None`.
        // SAFETY: test-only; no other test in this crate touches CLAUDECODE.
        unsafe { std::env::set_var("CLAUDECODE", "1") };
        let detected = agent_name();
        unsafe { std::env::remove_var("CLAUDECODE") };
        assert!(detected.is_some());
    }
}
