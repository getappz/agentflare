use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct ConfirmHit {
    pub file: PathBuf,
    pub line: usize,
    pub text: String,
}

pub fn search_crate_dir(crate_dir: &Path, crate_ident: &str) -> Vec<ConfirmHit> {
    let pattern = format!(r"\b{}::", regex::escape(crate_ident));
    if let Some(hits) = try_lean_ctx_grep(crate_dir, &pattern) {
        return hits;
    }
    fallback_scan(crate_dir, &pattern)
}

fn try_lean_ctx_grep(dir: &Path, pattern: &str) -> Option<Vec<ConfirmHit>> {
    let output = Command::new("lean-ctx")
        .arg("grep")
        .arg(pattern)
        .arg(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(parse_lean_ctx_grep_output(&text, dir))
}

/// `lean-ctx grep` prints a `N matches in M files [...]:` summary line, then
/// one `<path-relative-to-search-root>:<line> <text>` line per hit (space,
/// not colon, before the text).
fn parse_lean_ctx_grep_output(output: &str, search_root: &Path) -> Vec<ConfirmHit> {
    let mut hits = Vec::new();
    for line in output.lines().skip(1) {
        let Some((path_and_line, text)) = line.split_once(' ') else {
            continue;
        };
        let Some((path, line_no)) = path_and_line.rsplit_once(':') else {
            continue;
        };
        let Ok(line_no) = line_no.parse::<usize>() else {
            continue;
        };
        hits.push(ConfirmHit {
            file: search_root.join(path),
            line: line_no,
            text: text.to_string(),
        });
    }
    hits
}

/// Used when `lean-ctx` isn't on PATH. Recursively scans `*.rs` files under
/// `dir`, skipping `target/` and dotfile directories.
fn fallback_scan(dir: &Path, pattern: &str) -> Vec<ConfirmHit> {
    let Ok(re) = Regex::new(pattern) else {
        return Vec::new();
    };
    let mut hits = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if path.is_dir() {
                if name == "target" || name.starts_with('.') {
                    continue;
                }
                stack.push(path);
            } else if name.ends_with(".rs") {
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for (idx, line) in content.lines().enumerate() {
                    if re.is_match(line) {
                        hits.push(ConfirmHit {
                            file: path.clone(),
                            line: idx + 1,
                            text: line.trim().to_string(),
                        });
                    }
                }
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const SAMPLE_LEAN_CTX_GREP_OUTPUT: &str = "3 matches in 9 files [tests/queue_test.rs, tests/supervisor_test.rs, tests/worker_test.rs]:\ntests/queue_test.rs:1 use agentflare_jobs::{AgentJob, JobState, Queue};\ntests/supervisor_test.rs:1 use agentflare_jobs::{JobState, Supervisor};\ntests/worker_test.rs:1 use agentflare_jobs::{AgentJob, JobState, Queue, WorkerPool};\n";

    #[test]
    fn parses_lean_ctx_grep_output_skipping_the_summary_line() {
        let hits =
            parse_lean_ctx_grep_output(SAMPLE_LEAN_CTX_GREP_OUTPUT, Path::new("/ws/crates/x"));
        assert_eq!(hits.len(), 3);
        assert_eq!(
            hits[0].file,
            PathBuf::from("/ws/crates/x/tests/queue_test.rs")
        );
        assert_eq!(hits[0].line, 1);
        assert_eq!(
            hits[0].text,
            "use agentflare_jobs::{AgentJob, JobState, Queue};"
        );
        assert_eq!(
            hits[1].file,
            PathBuf::from("/ws/crates/x/tests/supervisor_test.rs")
        );
    }

    #[test]
    fn parses_empty_output_as_no_hits() {
        let hits = parse_lean_ctx_grep_output("0 matches in 0 files []:\n", Path::new("/ws"));
        assert!(hits.is_empty());
    }

    #[test]
    fn fallback_scan_finds_matches_and_skips_target_dir() {
        let dir = tempfile::tempdir().unwrap();
        // `lean-ctx grep` honours .gitignore (like a real repo's would), so
        // a fixture .gitignore keeps both backends skipping `target/` —
        // otherwise lean-ctx's grep would include the generated file and the
        // "one hit" expectation would differ by environment.
        std::fs::write(dir.path().join(".gitignore"), "target\n").unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "use my_crate::Thing;\nfn f() {}\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::write(
            dir.path().join("target").join("generated.rs"),
            "use my_crate::Other;\n",
        )
        .unwrap();

        let hits = search_crate_dir(dir.path(), "my_crate");
        // Only counts if lean-ctx isn't installed in the test environment;
        // assert on content, not on which path was taken.
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one hit, got: {:?}",
            hits.iter().map(|h| &h.file).collect::<Vec<_>>()
        );
        assert_eq!(hits[0].file, dir.path().join("lib.rs"));
        assert_eq!(hits[0].line, 1);
    }
}
