//! Per-turn cost of a Claude Code reply. Its `total_cost_usd` is the running total of the whole
//! *session*, so a resumed turn also reports everything before it. A turn costs the total now less
//! the total kept for the same session after its last turn; a new session costs its total; a
//! resumed session with nothing kept has an unknown cost (`None`), never its whole running total.
//! Only Claude Code reports a cumulative total; other agents' totals are not assumed to be.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FILE: &str = "claude-session-costs.json";
const VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Ledger {
    version: u32,
    /// Session id -> running total (USD) after its last turn.
    #[serde(default)]
    totals: BTreeMap<String, Entry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    total: f64,
    /// Unix seconds of the turn that set `total`; entries older than [`KEEP_SECS`] are dropped.
    at: u64,
}

/// A session idle this long is not resumed again, so its total is not worth keeping.
const KEEP_SECS: u64 = 30 * 24 * 3600;

/// How a Claude launch relates to an earlier session, read from its CLI args.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Launch {
    /// `--resume`/`--continue`: the turn continues an earlier session.
    pub resumed: bool,
    /// `--resume <id> --fork-session`: the earlier session the new one copied.
    pub forked_from: Option<String>,
}

/// `Some` for any Claude Code launch (its reported total is the session's running total, so even a
/// new session's total must be kept for its next turn); `None` for other agents.
pub(crate) fn launch_from_args(agent: &str, args: &[String]) -> Option<Launch> {
    if agent_registry::agent_by_name(agent) != Some(agent_registry::Agent::ClaudeCode) {
        return None;
    }
    let mut launch = Launch::default();
    let mut fork = false;
    let mut resume_id = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--resume" | "-r" => {
                launch.resumed = true;
                resume_id = iter.clone().next().filter(|v| !v.starts_with('-')).cloned();
            }
            "--continue" | "-c" => launch.resumed = true,
            "--fork-session" => fork = true,
            a if a.starts_with("--resume=") => {
                launch.resumed = true;
                resume_id = a.strip_prefix("--resume=").map(str::to_string);
            }
            _ => {}
        }
    }
    if fork {
        launch.forked_from = resume_id;
    }
    Some(launch)
}

/// Where the ledger lives.
pub(crate) fn default_path() -> PathBuf {
    crate::paths::agentflare_dir().join(FILE)
}

fn newer_version(bytes: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    serde_json::from_slice::<Version>(bytes).is_ok_and(|v| v.version > VERSION)
}

/// `None` = unreadable or from a newer version: leave the file alone.
fn load(path: &Path) -> Option<Ledger> {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Ledger>(&bytes) {
            Ok(l) if l.version == VERSION => Some(l),
            // A newer version may change `totals`' shape, so judge it by `version` alone.
            _ if newer_version(&bytes) => None,
            _ => Some(Ledger {
                version: VERSION,
                ..Default::default()
            }),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(Ledger {
            version: VERSION,
            ..Default::default()
        }),
        Err(_) => None,
    }
}

/// Exclusive lock on `<ledger>.lock`, held until the returned file is dropped.
fn lock(path: &Path) -> Option<std::fs::File> {
    use fs2::FileExt;
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(name)
        .ok()?;
    file.lock_exclusive().ok()?;
    Some(file)
}

/// The cost of one turn of `session`, given Claude's reported cumulative `total`. Records the new
/// total for the next turn. Invalid totals (non-finite, negative) cost `None` and record nothing.
pub(crate) fn turn_cost(path: &Path, session: &str, total: f64, launch: &Launch) -> Option<f64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    turn_cost_at(path, session, total, launch, now)
}

fn turn_cost_at(path: &Path, session: &str, total: f64, launch: &Launch, now: u64) -> Option<f64> {
    if !total.is_finite() || total < 0.0 {
        return None;
    }
    // Daemon, MCP handler and CLI runs are separate processes: serialise the read-modify-write
    // on a sibling lock file (the data file itself is replaced by rename). Without a lock the
    // turn is still priced, just not safely recorded, so a failure here never loses the cost.
    let _lock = lock(path);
    let ledger = load(path);
    let kept = |id: &str| {
        ledger
            .as_ref()
            .and_then(|l| l.totals.get(id).map(|e| e.total))
    };
    let cost = match (&launch.forked_from, launch.resumed) {
        (Some(parent), _) => kept(parent).map(|b| (total - b).max(0.0)),
        (None, true) => kept(session).map(|b| (total - b).max(0.0)),
        (None, false) => Some(total),
    };
    if let Some(mut ledger) = ledger {
        ledger.totals.retain(|_, e| e.at + KEEP_SECS >= now);
        ledger
            .totals
            .insert(session.to_string(), Entry { total, at: now });
        if let Ok(bytes) = serde_json::to_vec_pretty(&ledger) {
            let _ = crate::atomic_fs::write_bytes_with_fallback(path, &bytes, None);
        }
    }
    cost
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEW: Launch = Launch {
        resumed: false,
        forked_from: None,
    };
    const RESUMED: Launch = Launch {
        resumed: true,
        forked_from: None,
    };

    fn p(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join(FILE)
    }

    fn kept(path: &Path, id: &str) -> Option<f64> {
        load(path)?.totals.get(id).map(|e| e.total)
    }

    #[test]
    fn new_session_costs_its_total_and_resumed_turns_cost_the_delta() {
        let dir = tempfile::tempdir().unwrap();
        let path = p(&dir);
        assert_eq!(turn_cost(&path, "s", 0.25, &NEW), Some(0.25));
        let second = turn_cost(&path, "s", 0.75, &RESUMED).unwrap();
        assert!((second - 0.5).abs() < 1e-12);
        let third = turn_cost(&path, "s", 1.0, &RESUMED).unwrap();
        assert!((third - 0.25).abs() < 1e-12);
        assert_eq!(kept(&path, "s"), Some(1.0));
    }

    #[test]
    fn resumed_without_history_is_unknown_then_tracked() {
        let dir = tempfile::tempdir().unwrap();
        let path = p(&dir);
        assert_eq!(turn_cost(&path, "old", 3.5, &RESUMED), None);
        assert_eq!(turn_cost(&path, "old", 3.75, &RESUMED), Some(0.25));
    }

    #[test]
    fn a_total_that_went_down_costs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = p(&dir);
        turn_cost(&path, "s", 2.0, &NEW);
        assert_eq!(turn_cost(&path, "s", 1.5, &RESUMED), Some(0.0));
    }

    #[test]
    fn a_fork_prices_from_the_parents_total() {
        let dir = tempfile::tempdir().unwrap();
        let path = p(&dir);
        turn_cost(&path, "parent", 1.0, &NEW);
        let fork = Launch {
            resumed: true,
            forked_from: Some("parent".into()),
        };
        let cost = turn_cost(&path, "child", 1.2, &fork).unwrap();
        assert!((cost - 0.2).abs() < 1e-12);
        assert_eq!(kept(&path, "parent"), Some(1.0), "parent untouched");
        assert_eq!(kept(&path, "child"), Some(1.2));
        let orphan = Launch {
            resumed: true,
            forked_from: Some("missing".into()),
        };
        assert_eq!(turn_cost(&path, "c2", 1.2, &orphan), None);
    }

    #[test]
    fn bad_totals_cost_nothing_and_record_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = p(&dir);
        for t in [f64::NAN, -1.0, f64::INFINITY] {
            assert_eq!(turn_cost(&path, "s", t, &NEW), None);
        }
        assert!(!path.exists());
    }

    #[test]
    fn damaged_file_is_replaced_and_newer_file_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = p(&dir);
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(turn_cost(&path, "s", 2.0, &RESUMED), None);
        assert_eq!(kept(&path, "s"), Some(2.0));

        let newer = br#"{"version": 2, "totals": {"s": 1.0}}"#;
        std::fs::write(&path, newer).unwrap();
        assert_eq!(turn_cost(&path, "s", 2.0, &RESUMED), None);
        assert_eq!(turn_cost(&path, "n", 0.5, &NEW), Some(0.5));
        assert_eq!(std::fs::read(&path).unwrap(), newer);
    }

    #[test]
    fn a_plain_new_launch_then_a_resume_yields_the_delta() {
        let dir = tempfile::tempdir().unwrap();
        let path = p(&dir);
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let first = launch_from_args("claude-code", &a(&["--model=x"])).unwrap();
        assert_eq!(turn_cost(&path, "s", 0.4, &first), Some(0.4));
        let second = launch_from_args("claude-code", &a(&["--resume", "s"])).unwrap();
        let cost = turn_cost(&path, "s", 0.9, &second).unwrap();
        assert!((cost - 0.5).abs() < 1e-12);
    }

    #[test]
    fn concurrent_writers_do_not_lose_each_others_totals() {
        let dir = tempfile::tempdir().unwrap();
        let path = p(&dir);
        std::thread::scope(|scope| {
            for i in 0..8 {
                let path = &path;
                scope.spawn(move || turn_cost(path, &format!("s{i}"), 1.0, &NEW));
            }
        });
        for i in 0..8 {
            assert_eq!(kept(&path, &format!("s{i}")), Some(1.0), "s{i}");
        }
    }

    #[test]
    fn stale_sessions_are_dropped_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = p(&dir);
        turn_cost_at(&path, "old", 1.0, &NEW, 1_000);
        turn_cost_at(&path, "new", 2.0, &NEW, 1_000 + KEEP_SECS + 1);
        assert_eq!(kept(&path, "old"), None);
        assert_eq!(kept(&path, "new"), Some(2.0));
    }

    #[test]
    fn launch_args_are_classified() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            launch_from_args("claude-code", &a(&["--model=x"])),
            Some(Launch::default())
        );
        assert_eq!(launch_from_args("codex", &a(&["--resume", "s"])), None);
        assert_eq!(
            launch_from_args("claude-code", &a(&["--resume", "s"])),
            Some(Launch {
                resumed: true,
                forked_from: None
            })
        );
        assert_eq!(
            launch_from_args("claude-code", &a(&["--resume=s", "--fork-session"])),
            Some(Launch {
                resumed: true,
                forked_from: Some("s".into())
            })
        );
        assert_eq!(
            launch_from_args("claude-code", &a(&["-c"])),
            Some(Launch {
                resumed: true,
                forked_from: None
            })
        );
        assert_eq!(
            launch_from_args("claude-code", &a(&["--session-id=s"])),
            Some(Launch::default())
        );
    }
}
