// Progress/stall detection for a running headless agent -- included from
// `agent_launch.rs`. The idle-output timer there only catches a *silent*
// hang; an agent stuck in a chatty loop (e.g. "rate limited, retrying..."
// every few seconds) keeps resetting it and would hold its claim until the
// 6h hard cap. This adds two conservative signals:
//
// - stall: no worktree change (HEAD + `git status` fingerprint) AND no novel
//   output line for `window`, AND the recent output is a handful of lines
//   repeating. Long legitimate runs (tests, builds) print varied output, so
//   they never trip it.
// - exhaustion mid-run: the agent keeps printing a credit/quota-exhausted
//   error (see `auth_runner::classify_failure`) -- seen at least
//   `EXHAUSTION_HITS` times, spread over time, with the worktree unchanged
//   since the first sighting.

/// Leads the failure text of a run the stall detector killed (see
/// `run_headless_impl`); `auth_runner::skips_step_retry` keys on it.
pub(crate) const STALLED_MARKER: &str = "stalled — no worktree change and no new output";

/// Env override for the stall window, in seconds (`0` disables).
pub(crate) const STALL_WINDOW_ENV: &str = "AGENTFLARE_STALL_WINDOW_SECS";
/// Env override for how often the worktree fingerprint is taken, seconds.
pub(crate) const PROGRESS_CHECK_ENV: &str = "AGENTFLARE_PROGRESS_CHECK_SECS";
/// `0|false|off|no` disables the mid-run exhaustion kill.
pub(crate) const EXHAUSTION_KILL_ENV: &str = "AGENTFLARE_EXHAUSTION_KILL";
pub(crate) const DEFAULT_STALL_WINDOW_SECS: u64 = 45 * 60;
pub(crate) const DEFAULT_PROGRESS_CHECK_SECS: u64 = 5 * 60;

/// Recent normalized lines considered for repetition.
const RECENT_LINES: usize = 12;
/// At most this many distinct lines among `RECENT_LINES` counts as a loop.
const MAX_DISTINCT_IN_LOOP: usize = 3;
/// Distinct normalized lines remembered for novelty.
const SEEN_CAP: usize = 1024;
/// Normalized lines are truncated to this many chars.
const NORMALIZED_MAX: usize = 200;
/// Exhaustion lines longer than this are more likely file content echoed in
/// a transcript than the agent's own error.
const EXHAUSTION_LINE_MAX: usize = 400;
/// Sightings needed before an exhaustion kill...
const EXHAUSTION_HITS: u32 = 3;
/// ...each at least this far apart (a file dump prints them all at once; a
/// retry loop spreads them out).
const EXHAUSTION_HIT_SPACING: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy)]
pub(crate) struct StallConfig {
    /// `Duration::ZERO` disables stall detection.
    pub window: Duration,
    pub check_every: Duration,
    pub exhaustion_kill: bool,
}

impl StallConfig {
    pub(crate) fn from_env() -> Self {
        let secs = |key: &str, default: u64| {
            std::env::var(key)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(default)
        };
        let exhaustion_kill = !std::env::var(EXHAUSTION_KILL_ENV).is_ok_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        });
        Self {
            window: Duration::from_secs(secs(STALL_WINDOW_ENV, DEFAULT_STALL_WINDOW_SECS)),
            check_every: Duration::from_secs(
                secs(PROGRESS_CHECK_ENV, DEFAULT_PROGRESS_CHECK_SECS).max(1),
            ),
            exhaustion_kill,
        }
    }
}

/// Lowercases, collapses whitespace, and replaces every number-like token
/// (at least a third digits: counters, timestamps, ids, byte counts) with
/// `#`, so "attempt 3 of 10 at 12:01:07" and "attempt 4 of 10 at 12:01:19"
/// compare equal while `test case_1` and `test case_2` stay distinct.
pub(crate) fn normalize_line(line: &str) -> String {
    let mut out = String::new();
    for token in line.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        let mut word = String::new();
        let flush = |word: &mut String, out: &mut String| {
            let digits = word.chars().filter(char::is_ascii_digit).count();
            if digits > 0 && digits * 3 >= word.chars().count() {
                out.push('#');
            } else {
                out.push_str(&word.to_lowercase());
            }
            word.clear();
        };
        for c in token.chars() {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                word.push(c);
            } else {
                flush(&mut word, &mut out);
                out.push(c);
            }
        }
        flush(&mut word, &mut out);
        if out.len() >= NORMALIZED_MAX {
            break;
        }
    }
    out.chars().take(NORMALIZED_MAX).collect()
}

/// Whether an output line may be the agent's own error rather than content
/// it's echoing: short, and (for a JSON event line) error-shaped.
fn exhaustion_candidate(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.len() > EXHAUSTION_LINE_MAX {
        return false;
    }
    if trimmed.starts_with('{') {
        let lower = trimmed.to_ascii_lowercase();
        return lower.contains("\"is_error\":true")
            || lower.contains("\"type\":\"error\"")
            || lower.contains("api_retry");
    }
    true
}

/// Output-side progress state, fed by both reader threads.
#[derive(Debug)]
pub(crate) struct OutputProgress {
    partial: [String; 2],
    recent: std::collections::VecDeque<String>,
    seen: std::collections::HashSet<String>,
    seen_order: std::collections::VecDeque<String>,
    last_novel_at: Instant,
    exhaustion_line: Option<String>,
    exhaustion_hits: u32,
    last_exhaustion_hit: Option<Instant>,
}

impl OutputProgress {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            partial: [String::new(), String::new()],
            recent: Default::default(),
            seen: Default::default(),
            seen_order: Default::default(),
            last_novel_at: now,
            exhaustion_line: None,
            exhaustion_hits: 0,
            last_exhaustion_hit: None,
        }
    }

    /// Feeds a chunk read from stream `stream` (0 = stdout, 1 = stderr).
    pub(crate) fn feed(&mut self, stream: usize, chunk: &str, now: Instant) {
        let buf = &mut self.partial[stream.min(1)];
        buf.push_str(chunk);
        let Some(last_nl) = buf.rfind('\n') else {
            // Bound a runaway line with no newline.
            if buf.len() > 64 * 1024 {
                let line = std::mem::take(buf);
                self.line(&line, now);
            }
            return;
        };
        let complete: String = buf.drain(..=last_nl).collect();
        for line in complete.lines() {
            self.line(line, now);
        }
    }

    fn line(&mut self, line: &str, now: Instant) {
        let normalized = normalize_line(line);
        if normalized.is_empty() {
            return;
        }
        if self.seen.insert(normalized.clone()) {
            self.last_novel_at = now;
            self.seen_order.push_back(normalized.clone());
            if self.seen_order.len() > SEEN_CAP
                && let Some(old) = self.seen_order.pop_front()
            {
                self.seen.remove(&old);
            }
        }
        self.recent.push_back(normalized);
        if self.recent.len() > RECENT_LINES {
            self.recent.pop_front();
        }
        if exhaustion_candidate(line)
            && matches!(
                crate::auth_runner::classify_failure(line),
                crate::auth_runner::AgentFailure::CreditExhausted
                    | crate::auth_runner::AgentFailure::QuotaWindowExhausted { .. }
            )
            && self
                .last_exhaustion_hit
                .is_none_or(|t| now.duration_since(t) >= EXHAUSTION_HIT_SPACING)
        {
            self.exhaustion_hits += 1;
            self.last_exhaustion_hit = Some(now);
            self.exhaustion_line = Some(line.trim().to_string());
        }
    }

    /// The last `RECENT_LINES` lines are a handful of lines repeating.
    pub(crate) fn is_repetitive(&self) -> bool {
        if self.recent.len() < RECENT_LINES {
            return false;
        }
        let distinct: std::collections::HashSet<&String> = self.recent.iter().collect();
        distinct.len() <= MAX_DISTINCT_IN_LOOP
    }

    pub(crate) fn last_novel_at(&self) -> Instant {
        self.last_novel_at
    }

    /// The exhaustion error line once it has been sighted `EXHAUSTION_HITS`
    /// times (spaced out).
    pub(crate) fn exhausted(&self) -> Option<&str> {
        (self.exhaustion_hits >= EXHAUSTION_HITS)
            .then_some(self.exhaustion_line.as_deref())
            .flatten()
    }

    pub(crate) fn exhaustion_first_seen(&self) -> bool {
        self.exhaustion_hits >= 1
    }
}

/// The stall verdict, pure: no progress (worktree change or novel output)
/// for `window`, and the output is looping.
pub(crate) fn is_stalled(
    now: Instant,
    last_worktree_change: Instant,
    last_novel_output: Instant,
    repetitive: bool,
    window: Duration,
) -> bool {
    if window.is_zero() || !repetitive {
        return false;
    }
    let last_progress = last_worktree_change.max(last_novel_output);
    now.duration_since(last_progress) >= window
}

/// Cheap fingerprint of a worktree's state: HEAD plus `git status
/// --porcelain`, plus size+mtime of each listed path (so continued edits to
/// an already-modified file still register). `None` when `dir` isn't a git
/// worktree. `GIT_OPTIONAL_LOCKS=0` keeps this read-only probe from taking
/// the index lock the agent's own git commands need.
pub(crate) fn worktree_fingerprint(dir: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let git = |args: &[&str]| {
        flare_process::command("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| o.stdout)
    };
    let head = git(&["rev-parse", "HEAD"]).unwrap_or_default();
    let status = git(&["status", "--porcelain", "--untracked-files=all"])?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    head.hash(&mut hasher);
    status.hash(&mut hasher);
    for line in String::from_utf8_lossy(&status).lines().take(500) {
        let path = line
            .get(3..)
            .unwrap_or("")
            .rsplit(" -> ")
            .next()
            .unwrap_or("");
        if let Ok(meta) = std::fs::metadata(dir.join(path.trim_matches('"'))) {
            meta.len().hash(&mut hasher);
            meta.modified().ok().hash(&mut hasher);
        }
    }
    Some(hasher.finish())
}

/// Periodic worktree probe for the wait loop.
pub(crate) struct WorktreeProgress {
    dir: Option<std::path::PathBuf>,
    last_fingerprint: Option<u64>,
    last_check: Instant,
    pub(crate) last_change: Instant,
    /// Fingerprint when an exhaustion line was first seen.
    exhaustion_baseline: Option<Option<u64>>,
}

impl WorktreeProgress {
    pub(crate) fn new(dir: Option<std::path::PathBuf>, now: Instant) -> Self {
        let last_fingerprint = dir.as_deref().and_then(worktree_fingerprint);
        Self {
            dir,
            last_fingerprint,
            last_check: now,
            last_change: now,
            exhaustion_baseline: None,
        }
    }

    fn fingerprint(&self) -> Option<u64> {
        self.dir.as_deref().and_then(worktree_fingerprint)
    }

    /// Re-fingerprints every `every`; a change counts as progress.
    pub(crate) fn poll(&mut self, now: Instant, every: Duration) {
        if now.duration_since(self.last_check) < every {
            return;
        }
        self.last_check = now;
        let current = self.fingerprint();
        if current != self.last_fingerprint {
            self.last_fingerprint = current;
            self.last_change = now;
        }
    }

    /// Records the worktree state at the first exhaustion sighting.
    pub(crate) fn note_exhaustion_seen(&mut self) {
        if self.exhaustion_baseline.is_none() {
            self.exhaustion_baseline = Some(self.fingerprint());
        }
    }

    /// Unchanged since the first exhaustion sighting.
    pub(crate) fn unchanged_since_exhaustion(&self) -> bool {
        self.exhaustion_baseline
            .is_some_and(|baseline| baseline == self.fingerprint())
    }
}
