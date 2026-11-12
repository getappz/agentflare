//! Preflight: does an installed agent CLI still accept the flags its
//! [`headless`](crate::headless) row passes? Probes `--help` once per binary
//! build so a renamed/dropped flag fails fast with a clear message instead of
//! an opaque dispatch error.

use crate::detect::run_with_timeout;
use crate::headless::required_flags;
use crate::registry::Agent;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

const HELP_TIMEOUT: Duration = Duration::from_secs(5);

/// Verdict of a flag probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagProbe {
    /// Help was readable and mentions every needle.
    Present,
    /// Help was readable and these needles are absent.
    Missing(Vec<String>),
    /// Couldn't tell (spawn failure, hang, crash, unrecognisable output).
    Unknown,
}

/// `(binary path, mtime)` of binaries already verified `Present`. Only
/// positive verdicts are cached: a transient failure must not stick.
static VERIFIED: Mutex<Option<HashSet<(PathBuf, u64)>>> = Mutex::new(None);

/// True when `needle` appears in `help` as a whole flag (`--json` must not
/// match `--json-schema`).
fn mentions(help: &str, needle: &str) -> bool {
    help.match_indices(needle).any(|(i, _)| {
        help[i + needle.len()..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    })
}

/// Pure verdict over captured help text. Readable means it looks like a help
/// screen (`usage`/`options`); anything else is `Unknown`, never `Missing`.
fn judge(help: &str, needles: &[&str]) -> FlagProbe {
    let lower = help.to_ascii_lowercase();
    if !(lower.contains("usage") || lower.contains("options")) {
        return FlagProbe::Unknown;
    }
    let missing: Vec<String> = needles
        .iter()
        .filter(|n| !mentions(help, n))
        .map(|n| (*n).to_string())
        .collect();
    if missing.is_empty() {
        FlagProbe::Present
    } else {
        FlagProbe::Missing(missing)
    }
}

/// Preflight for a headless launch of `agent` at `binary`.
///
/// # Errors
///
/// `Err` with an "upgrade <agent>: ..." message only when `--help` was
/// readable and lacks a flag we pass. An undeterminable probe proceeds
/// (logged), and the cache means each binary build is probed once.
pub fn check_headless_flags(agent: Agent, display_name: &str, binary: &Path) -> Result<(), String> {
    check_with(agent, display_name, binary, HELP_TIMEOUT)
}

fn check_with(
    agent: Agent,
    display_name: &str,
    binary: &Path,
    timeout: Duration,
) -> Result<(), String> {
    let Some((help_args, needles)) = required_flags(agent) else {
        return Ok(());
    };
    let key = std::fs::metadata(binary)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| (binary.to_path_buf(), d.as_secs()));
    if let Some(k) = &key
        && VERIFIED
            .lock()
            .is_ok_and(|g| g.as_ref().is_some_and(|s| s.contains(k)))
    {
        return Ok(());
    }
    let verdict = run_with_timeout(binary, &help_args, timeout)
        .map_or(FlagProbe::Unknown, |help| judge(&help, &needles));
    match verdict {
        FlagProbe::Present => {
            if let (Some(key), Ok(mut g)) = (key, VERIFIED.lock()) {
                g.get_or_insert_with(HashSet::new).insert(key);
            }
            Ok(())
        }
        FlagProbe::Missing(missing) => Err(format!(
            "upgrade {display_name}: {} no longer lists {} in --help",
            binary.display(),
            missing.join(", ")
        )),
        FlagProbe::Unknown => {
            eprintln!("note: could not verify {display_name} flags via --help; proceeding");
            Ok(())
        }
    }
}

#[cfg(test)]
mod judge_tests {
    use super::*;

    #[test]
    fn judge_is_tri_state() {
        let help = "Usage: x\n  --sandbox  --json\n";
        assert_eq!(judge(help, &["--sandbox", "--json"]), FlagProbe::Present);
        assert_eq!(
            judge(help, &["--sandbox", "--nope"]),
            FlagProbe::Missing(vec!["--nope".into()])
        );
        assert_eq!(judge("garbage", &["--json"]), FlagProbe::Unknown);
    }

    #[test]
    fn mentions_requires_whole_flag() {
        assert!(mentions("  --json  ", "--json"));
        assert!(mentions("--json", "--json"));
        assert!(!mentions("--json-schema", "--json"));
    }
}

// Fake CLIs are `#!/bin/sh` scripts: `.bat`/`.cmd` stubs aren't reliably
// spawnable via `Command` on Windows, so the process-level tests are
// unix-only; the verdict logic above is covered on every platform.
#[cfg(all(test, unix))]
mod fake_cli_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_cli(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("af-flags-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    const SHORT: Duration = Duration::from_millis(300);

    fn codex_help(flags: &str) -> String {
        format!("echo 'Usage: codex exec [OPTIONS]'; echo '{flags}'")
    }

    #[test]
    fn present_flags_pass_and_are_cached() {
        let bin = fake_cli("ok", &codex_help("--sandbox <MODE>  --json"));
        assert!(check_with(Agent::Codex, "Codex", &bin, SHORT).is_ok());
        // Same (path, mtime): cached, so a missing flag is not re-probed.
        let mtime = std::fs::metadata(&bin).unwrap().modified().unwrap();
        std::fs::write(&bin, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&bin)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        assert!(check_with(Agent::Codex, "Codex", &bin, SHORT).is_ok());
    }

    #[test]
    fn missing_flag_fails_fast_naming_it() {
        let bin = fake_cli("missing", &codex_help("--sandbox <MODE>"));
        let err = check_with(Agent::Codex, "Codex", &bin, SHORT).unwrap_err();
        assert!(
            err.contains("upgrade Codex") && err.contains("--json"),
            "{err}"
        );
    }

    #[test]
    fn crashing_help_is_unknown_and_proceeds() {
        let bin = fake_cli("crash", "exit 1");
        assert!(check_with(Agent::Codex, "Codex", &bin, SHORT).is_ok());
    }

    #[test]
    fn hanging_help_times_out_and_proceeds() {
        let bin = fake_cli("hang", "sleep 30");
        let start = std::time::Instant::now();
        assert!(check_with(Agent::Codex, "Codex", &bin, SHORT).is_ok());
        assert!(start.elapsed() < Duration::from_secs(10));
    }
}
