// Detection engine for `agentflare agents list`/`doctor`: PATH search,
// version resolution, and mtime-keyed caching. Kept free of println!/CLI
// concerns so it's fully unit-testable — src/agents.rs owns rendering.
use crate::agent_registry::{AgentSpec, Tier};
use crate::state::VersionCacheEntry;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

// PATH is process-global — every test in this file that mutates it (via
// `with_temp_path_dir` in `find_binary_tests`/`detect_all_tests`) or relies
// on the real one (`resolve_version_tests::run_version_command_captures_
// real_process_output`) must serialize against this single shared lock.
// Two separate `Mutex` instances would not actually serialize anything.
#[cfg(test)]
static PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Search PATH for the first name in `names` that resolves to a file.
/// Manual walk (not the `which` crate) to avoid a new dependency — mirrors
/// the approach used by `agents-cli`'s `findInPath` and `caam`'s
/// `findBinary`, minus their shims-dir exclusion (agentflare has no shims
/// directory yet).
pub fn find_binary(names: &[&str]) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
            #[cfg(windows)]
            for ext in [".exe", ".cmd", ".bat"] {
                let candidate = dir.join(format!("{name}{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod find_binary_tests {
    use super::*;

    // PATH is process-global — serialize every test that mutates it against
    // the single shared `super::PATH_LOCK`, same pattern as
    // paths::test_support::GLOBAL_STATE_LOCK.

    fn with_temp_path_dir(f: impl FnOnce(&Path)) {
        let _guard = super::PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("agentflare-test-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let original = std::env::var_os("PATH");
        std::env::set_var("PATH", &dir);
        f(&dir);
        match original {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_binary_present_in_a_path_dir() {
        with_temp_path_dir(|dir| {
            std::fs::write(dir.join("stubagent"), "").unwrap();
            let found = find_binary(&["stubagent"]).unwrap();
            assert_eq!(found, dir.join("stubagent"));
        });
    }

    #[test]
    fn returns_none_when_not_on_path() {
        with_temp_path_dir(|_dir| {
            assert!(find_binary(&["definitely-not-installed-xyz"]).is_none());
        });
    }

    #[test]
    fn tries_names_in_order_and_returns_first_match() {
        with_temp_path_dir(|dir| {
            std::fs::write(dir.join("second-name"), "").unwrap();
            let found = find_binary(&["first-name", "second-name"]).unwrap();
            assert_eq!(found, dir.join("second-name"));
        });
    }
}

/// Find the first `\d+\.\d+\.\d+`-shaped substring in `text` (a `--version`
/// command's combined stdout+stderr). Hand-rolled instead of pulling in the
/// `regex` crate for one pattern.
pub fn extract_version(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    for start in 0..chars.len() {
        if !chars[start].is_ascii_digit() {
            continue;
        }
        let mut i = start;
        let mut dot_groups = 0;
        let mut end = start;
        while i < chars.len() {
            if chars[i].is_ascii_digit() {
                i += 1;
                end = i;
            } else if chars[i] == '.' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
                dot_groups += 1;
                i += 1;
            } else {
                break;
            }
        }
        if dot_groups >= 2 {
            return Some(chars[start..end].iter().collect());
        }
    }
    None
}

#[cfg(test)]
mod extract_version_tests {
    use super::*;

    #[test]
    fn extracts_version_from_prefixed_output() {
        assert_eq!(extract_version("claude-code/1.2.3"), Some("1.2.3".to_string()));
    }

    #[test]
    fn extracts_version_embedded_in_a_sentence() {
        assert_eq!(extract_version("codex cli version 0.128.0 (build abc)"), Some("0.128.0".to_string()));
    }

    #[test]
    fn returns_none_for_two_part_version() {
        assert_eq!(extract_version("v1.2"), None);
    }

    #[test]
    fn returns_none_when_no_digits_present() {
        assert_eq!(extract_version("unknown command"), None);
    }

    #[test]
    fn returns_first_match_when_multiple_numbers_present() {
        assert_eq!(extract_version("built with node 20.11.0 for app 1.2.3"), Some("20.11.0".to_string()));
    }
}

/// Runs a version-probe command and returns its combined stdout+stderr, or
/// an error message. Abstracted behind a trait so `resolve_version_with`
/// can be tested with a fake runner instead of spawning real processes
/// (real agent binaries aren't installed in test environments, and `.bat`/
/// `.cmd` stub scripts aren't reliably spawnable via `std::process::Command`
/// on Windows).
pub trait VersionRunner {
    fn run(&self, binary: &Path, args: &[&str]) -> Result<String, String>;
}

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

#[allow(dead_code)]
pub struct RealVersionRunner;

impl VersionRunner for RealVersionRunner {
    fn run(&self, binary: &Path, args: &[&str]) -> Result<String, String> {
        run_version_command(binary, args)
    }
}

/// Spawn `binary args...` on a helper thread and wait up to
/// `VERSION_TIMEOUT`. A hung child on timeout is not killed here — killing
/// cross-platform requires platform-specific process-group handling, which
/// is overkill for an occasional `--version` probe; the helper thread is
/// simply abandoned and the timeout error returned.
fn run_version_command(binary: &Path, args: &[&str]) -> Result<String, String> {
    let binary = binary.to_path_buf();
    let binary_for_thread = binary.clone();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let result = std::process::Command::new(&binary_for_thread)
            .args(&args)
            .output()
            .map_err(|e| format!("failed to spawn {}: {e}", binary_for_thread.display()))
            .map(|output| {
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
            });
        let _ = tx.send(result);
    });

    match rx.recv_timeout(VERSION_TIMEOUT) {
        Ok(Ok(text)) if !text.trim().is_empty() => Ok(text),
        Ok(Ok(_)) => Err(format!("{} produced no output", binary.display())),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(format!("{} timed out after {VERSION_TIMEOUT:?}", binary.display())),
    }
}

/// Resolve an agent's version, using `cache` when the binary path and mtime
/// both match a prior resolution, otherwise invoking `runner`. A failed or
/// unparseable resolution is never written to `cache` — a transient
/// `--version` failure must not stick forever.
pub fn resolve_version_with(
    runner: &dyn VersionRunner,
    agent_key: &str,
    binary_path: &Path,
    version_args: &[&str],
    cache: &mut HashMap<String, VersionCacheEntry>,
) -> Result<String, String> {
    let mtime = std::fs::metadata(binary_path)
        .and_then(|m| m.modified())
        .map_err(|e| format!("could not stat {}: {e}", binary_path.display()))?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let binary_path_str = binary_path.to_string_lossy().into_owned();

    if let Some(entry) = cache.get(agent_key) {
        if entry.binary_path == binary_path_str && entry.mtime == mtime {
            return Ok(entry.version.clone());
        }
    }

    let raw = runner.run(binary_path, version_args)?;
    let version = extract_version(&raw)
        .ok_or_else(|| format!("could not parse a version from output: {raw:?}"))?;

    cache.insert(
        agent_key.to_string(),
        VersionCacheEntry { binary_path: binary_path_str, mtime, version: version.clone() },
    );
    Ok(version)
}

#[allow(dead_code)]
pub fn resolve_version(
    agent_key: &str,
    binary_path: &Path,
    version_args: &[&str],
    cache: &mut HashMap<String, VersionCacheEntry>,
) -> Result<String, String> {
    resolve_version_with(&RealVersionRunner, agent_key, binary_path, version_args, cache)
}

#[cfg(test)]
mod resolve_version_tests {
    use super::*;
    use std::io::Write;

    struct FakeRunner {
        response: Result<String, String>,
    }

    impl VersionRunner for FakeRunner {
        fn run(&self, _binary: &Path, _args: &[&str]) -> Result<String, String> {
            self.response.clone()
        }
    }

    /// A real file on disk so `fs::metadata` succeeds — its content is
    /// irrelevant since `FakeRunner` never actually executes it.
    fn temp_binary_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agentflare-test-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"stub").unwrap();
        path
    }

    #[test]
    fn cache_miss_spawns_and_caches_result() {
        let binary = temp_binary_file("agent-a");
        let runner = FakeRunner { response: Ok("agent-a version 1.2.3".to_string()) };
        let mut cache = HashMap::new();

        let version = resolve_version_with(&runner, "agent-a", &binary, &["--version"], &mut cache).unwrap();

        assert_eq!(version, "1.2.3");
        assert_eq!(cache.get("agent-a").unwrap().version, "1.2.3");
    }

    #[test]
    fn cache_hit_does_not_call_runner_again() {
        let binary = temp_binary_file("agent-b");
        let mtime = std::fs::metadata(&binary).unwrap().modified().unwrap()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let mut cache = HashMap::new();
        cache.insert(
            "agent-b".to_string(),
            VersionCacheEntry {
                binary_path: binary.to_string_lossy().into_owned(),
                mtime,
                version: "9.9.9".to_string(),
            },
        );
        // A runner that would panic if called proves the cache path short-circuits.
        struct PanicRunner;
        impl VersionRunner for PanicRunner {
            fn run(&self, _binary: &Path, _args: &[&str]) -> Result<String, String> {
                panic!("runner should not be called on a cache hit");
            }
        }

        let version = resolve_version_with(&PanicRunner, "agent-b", &binary, &["--version"], &mut cache).unwrap();
        assert_eq!(version, "9.9.9");
    }

    #[test]
    fn stale_mtime_invalidates_cache_and_respawns() {
        let binary = temp_binary_file("agent-c");
        let mut cache = HashMap::new();
        cache.insert(
            "agent-c".to_string(),
            VersionCacheEntry {
                binary_path: binary.to_string_lossy().into_owned(),
                mtime: 1, // deliberately wrong — real mtime is much larger
                version: "0.0.0".to_string(),
            },
        );
        let runner = FakeRunner { response: Ok("2.0.0".to_string()) };

        let version = resolve_version_with(&runner, "agent-c", &binary, &["--version"], &mut cache).unwrap();
        assert_eq!(version, "2.0.0");
    }

    #[test]
    fn failed_resolution_is_not_persisted_to_cache() {
        let binary = temp_binary_file("agent-d");
        let runner = FakeRunner { response: Err("boom".to_string()) };
        let mut cache = HashMap::new();

        let result = resolve_version_with(&runner, "agent-d", &binary, &["--version"], &mut cache);

        assert!(result.is_err());
        assert!(!cache.contains_key("agent-d"), "a failed resolution must not be cached");
    }

    #[test]
    fn unparseable_success_output_is_not_persisted_to_cache() {
        let binary = temp_binary_file("agent-e");
        let runner = FakeRunner { response: Ok("no version information here".to_string()) };
        let mut cache = HashMap::new();

        let result = resolve_version_with(&runner, "agent-e", &binary, &["--version"], &mut cache);

        assert!(result.is_err());
        assert!(
            !cache.contains_key("agent-e"),
            "a successful-but-unparseable resolution must not be cached"
        );
    }

    #[test]
    fn run_version_command_captures_real_process_output() {
        // Take the shared PATH_LOCK so this can't run concurrently with a
        // find_binary_tests/detect_all_tests test that has repointed PATH
        // to a temp-only directory — this test needs the real PATH intact.
        let _guard = PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // The one real-spawn test: `rustc` is guaranteed on PATH inside any
        // `cargo test` invocation, so this is portable without a stub binary.
        let output = run_version_command(Path::new("rustc"), &["--version"]).unwrap();
        assert!(output.to_lowercase().contains("rustc"));
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DetectedAgent {
    pub id: &'static str,
    pub display_name: &'static str,
    pub binary_path: String,
    pub version: Option<String>,
    pub status: &'static str,
    pub error: Option<String>,
}

/// Detect every `Tier::Cli` agent in `registry` that has a binary on PATH,
/// resolving its version via `runner`. `Tier::Extension` entries are always
/// skipped — they have no `binary_names` to search for. Agents not found on
/// PATH are omitted entirely (installed-only, per the design doc).
pub fn detect_all_with(
    registry: &[AgentSpec],
    cache: &mut HashMap<String, VersionCacheEntry>,
    runner: &dyn VersionRunner,
) -> Vec<DetectedAgent> {
    let mut results = Vec::new();
    for spec in registry {
        if spec.tier != Tier::Cli {
            continue;
        }
        let Some(binary_path) = find_binary(spec.binary_names) else {
            continue;
        };
        let agent_key = spec.id.as_str();
        let binary_path_str = binary_path.to_string_lossy().into_owned();
        match resolve_version_with(runner, agent_key, &binary_path, spec.version_args, cache) {
            Ok(version) => results.push(DetectedAgent {
                id: agent_key,
                display_name: spec.display_name,
                binary_path: binary_path_str,
                version: Some(version),
                status: "ready",
                error: None,
            }),
            Err(e) => results.push(DetectedAgent {
                id: agent_key,
                display_name: spec.display_name,
                binary_path: binary_path_str,
                version: None,
                status: "unknown",
                error: Some(e),
            }),
        }
    }
    results
}

#[allow(dead_code)]
pub fn detect_all(
    registry: &[AgentSpec],
    cache: &mut HashMap<String, VersionCacheEntry>,
) -> Vec<DetectedAgent> {
    detect_all_with(registry, cache, &RealVersionRunner)
}

#[cfg(test)]
mod detect_all_tests {
    use super::*;
    use crate::agent_registry::Agent;

    struct StubRunner;
    impl VersionRunner for StubRunner {
        fn run(&self, binary: &Path, _args: &[&str]) -> Result<String, String> {
            if binary.to_string_lossy().contains("broken") {
                Err("simulated failure".to_string())
            } else {
                Ok("1.0.0".to_string())
            }
        }
    }

    fn with_temp_path_dir(f: impl FnOnce(&Path)) {
        let _guard = super::PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("agentflare-test-detect-all-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let original = std::env::var_os("PATH");
        std::env::set_var("PATH", &dir);
        f(&dir);
        match original {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_extension_tier_and_not_found_cli_tier_agents() {
        with_temp_path_dir(|dir| {
            std::fs::write(dir.join("found-agent"), "").unwrap();
            let registry = [
                AgentSpec {
                    id: Agent::Aider,
                    display_name: "found",
                    tier: Tier::Cli,
                    binary_names: &["found-agent"],
                    version_args: &["--version"],
                },
                AgentSpec {
                    id: Agent::Codex,
                    display_name: "missing",
                    tier: Tier::Cli,
                    binary_names: &["not-on-path-xyz"],
                    version_args: &["--version"],
                },
                AgentSpec {
                    id: Agent::Cline,
                    display_name: "cline",
                    tier: Tier::Extension,
                    binary_names: &[],
                    version_args: &[],
                },
            ];
            let mut cache = HashMap::new();

            let detected = detect_all_with(&registry, &mut cache, &StubRunner);

            assert_eq!(detected.len(), 1);
            assert_eq!(detected[0].display_name, "found");
            assert_eq!(detected[0].version, Some("1.0.0".to_string()));
            assert_eq!(detected[0].status, "ready");
        });
    }

    #[test]
    fn found_but_unresolvable_version_reports_unknown_status_with_error() {
        with_temp_path_dir(|dir| {
            std::fs::write(dir.join("broken-agent"), "").unwrap();
            let registry = [AgentSpec {
                id: Agent::Aider,
                display_name: "broken",
                tier: Tier::Cli,
                binary_names: &["broken-agent"],
                version_args: &["--version"],
            }];
            let mut cache = HashMap::new();

            let detected = detect_all_with(&registry, &mut cache, &StubRunner);

            assert_eq!(detected.len(), 1);
            assert_eq!(detected[0].version, None);
            assert_eq!(detected[0].status, "unknown");
            assert_eq!(detected[0].error.as_deref(), Some("simulated failure"));
        });
    }
}
