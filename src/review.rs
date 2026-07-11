//! Multi-agent review consensus. Finders (external agents running /code-review,
//! ponytail, subagents, …) submit findings against a diff; agentflare's job is
//! the deterministic part: mechanically verify each cited `file:line` is a
//! changed line, cluster overlapping findings, and tag each cluster by
//! agreement + verification. No agent launching, no posting — just the
//! verifiable consensus core (see issue #142).
use rusqlite::{Connection, params};
use std::collections::{HashMap, HashSet};

/// Lines within this many rows of each other (same file) are treated as the
/// same finding — different agents rarely cite the exact same line for one
/// issue.
const CLUSTER_WINDOW: u32 = 3;

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS review_findings (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            repo       TEXT NOT NULL,
            pr         TEXT NOT NULL,
            agent      TEXT NOT NULL,
            file       TEXT NOT NULL,
            line       INTEGER NOT NULL,
            message    TEXT NOT NULL,
            severity   TEXT,
            category   TEXT,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS review_findings_round ON review_findings (repo, pr);
        CREATE TABLE IF NOT EXISTS score_events (
            repo        TEXT NOT NULL,
            pr          TEXT NOT NULL,
            agent       TEXT NOT NULL,
            total       INTEGER NOT NULL,
            verified    INTEGER NOT NULL,
            recorded_at INTEGER NOT NULL,
            PRIMARY KEY (repo, pr, agent)
        );",
    )
}

/// A finding as submitted by one finder.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Finding {
    pub file: String,
    pub line: u32,
    pub message: String,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
}

/// A finding read back from storage (carries its author).
#[derive(Debug, Clone)]
pub struct StoredFinding {
    pub agent: String,
    pub finding: Finding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Verdict {
    /// Verified location, ≥2 agents agree.
    Confirmed,
    /// Verified location, agents gave conflicting severities.
    Disputed,
    /// Verified location, a single agent.
    Unique,
    /// Cited line is not a changed line in the diff.
    Unverified,
}

impl Verdict {
    /// Ranking weight — higher sorts first in the report.
    fn rank(self) -> u8 {
        match self {
            Verdict::Confirmed => 3,
            Verdict::Disputed => 2,
            Verdict::Unique => 1,
            Verdict::Unverified => 0,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConsensusItem {
    pub verdict: Verdict,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub agents: Vec<String>,
    pub messages: Vec<String>,
    pub severities: Vec<String>,
}

// --- storage -----------------------------------------------------------------

/// Records `agent`'s findings for a review round, replacing any the same agent
/// submitted before (a re-run gives a fresh set, not duplicates).
pub fn submit(
    conn: &Connection,
    repo: &str,
    pr: &str,
    agent: &str,
    findings: &[Finding],
    now: i64,
) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM review_findings WHERE repo = ?1 AND pr = ?2 AND agent = ?3",
        params![repo, pr, agent],
    )?;
    for f in findings {
        conn.execute(
            "INSERT INTO review_findings (repo, pr, agent, file, line, message, severity, category, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![repo, pr, agent, f.file, f.line, f.message, f.severity, f.category, now],
        )?;
    }
    Ok(findings.len())
}

pub fn load(conn: &Connection, repo: &str, pr: &str) -> rusqlite::Result<Vec<StoredFinding>> {
    let mut stmt = conn.prepare(
        "SELECT agent, file, line, message, severity, category
         FROM review_findings WHERE repo = ?1 AND pr = ?2
         ORDER BY file, line",
    )?;
    let rows = stmt.query_map(params![repo, pr], |r| {
        Ok(StoredFinding {
            agent: r.get(0)?,
            finding: Finding {
                file: r.get(1)?,
                line: r.get(2)?,
                message: r.get(3)?,
                severity: r.get(4)?,
                category: r.get(5)?,
            },
        })
    })?;
    rows.collect()
}

pub fn clear(conn: &Connection, repo: &str, pr: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM review_findings WHERE repo = ?1 AND pr = ?2",
        params![repo, pr],
    )
}

// --- accuracy scoring --------------------------------------------------------

/// A finder's aggregate accuracy across recorded rounds.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentScore {
    pub agent: String,
    pub findings: u32,
    pub verified: u32,
    /// verified / findings, 0.0 when no findings.
    pub accuracy: f64,
    pub rounds: u32,
}

/// Records this round's per-agent tally (total findings, and how many cited a
/// real changed line) into `score_events`, upserting one row per agent so
/// re-recording the same round REPLACES its contribution rather than
/// double-counting. Returns the number of agents recorded.
pub fn record_round(
    conn: &Connection,
    repo: &str,
    pr: &str,
    findings: &[StoredFinding],
    changed: &HashMap<String, HashSet<u32>>,
    now: i64,
) -> rusqlite::Result<usize> {
    let mut per_agent: HashMap<&str, (u32, u32)> = HashMap::new();
    for sf in findings {
        let entry = per_agent.entry(sf.agent.as_str()).or_default();
        entry.0 += 1;
        if changed
            .get(&sf.finding.file)
            .is_some_and(|s| s.contains(&sf.finding.line))
        {
            entry.1 += 1;
        }
    }
    for (agent, (total, verified)) in &per_agent {
        conn.execute(
            "INSERT INTO score_events (repo, pr, agent, total, verified, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(repo, pr, agent) DO UPDATE SET
                 total = excluded.total,
                 verified = excluded.verified,
                 recorded_at = excluded.recorded_at",
            params![repo, pr, agent, total, verified, now],
        )?;
    }
    Ok(per_agent.len())
}

/// Aggregates recorded score events per agent (optionally scoped to `repo`),
/// ranked by accuracy then volume.
pub fn scores(conn: &Connection, repo: Option<&str>) -> rusqlite::Result<Vec<AgentScore>> {
    let mut stmt = conn.prepare(
        "SELECT agent, SUM(total), SUM(verified), COUNT(*)
         FROM score_events
         WHERE (?1 IS NULL OR repo = ?1)
         GROUP BY agent",
    )?;
    let mut out: Vec<AgentScore> = stmt
        .query_map(params![repo], |r| {
            let findings: i64 = r.get(1)?;
            let verified: i64 = r.get(2)?;
            let rounds: i64 = r.get(3)?;
            Ok(AgentScore {
                agent: r.get(0)?,
                findings: findings as u32,
                verified: verified as u32,
                accuracy: if findings > 0 {
                    verified as f64 / findings as f64
                } else {
                    0.0
                },
                rounds: rounds as u32,
            })
        })?
        .collect::<Result<_, _>>()?;
    out.sort_by(|a, b| {
        b.accuracy
            .partial_cmp(&a.accuracy)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.findings.cmp(&a.findings))
            .then_with(|| a.agent.cmp(&b.agent))
    });
    Ok(out)
}

// --- diff parsing (pure) -----------------------------------------------------

/// Parses a unified diff into the set of new-side line numbers present in each
/// file's hunks (added `+` and context lines). A finding is "verified" when it
/// cites one of these — i.e. it points at a line the diff actually shows.
pub fn changed_lines(diff: &str) -> HashMap<String, HashSet<u32>> {
    let mut out: HashMap<String, HashSet<u32>> = HashMap::new();
    let mut file: Option<String> = None;
    let mut new_line: u32 = 0;
    for raw in diff.lines() {
        if raw.starts_with("diff --git ") {
            // New file section. Clear the current file so the metadata lines
            // that follow (`index …`, `--- …`, mode/rename headers) aren't
            // mistaken for context lines of the PREVIOUS file — which would
            // insert phantom changed-line numbers and falsely verify findings.
            file = None;
        } else if let Some(path) = raw.strip_prefix("+++ ") {
            // "+++ b/src/foo.rs" → "src/foo.rs" ("/dev/null" for deletions).
            let p = path.trim().trim_start_matches("b/");
            file = (p != "/dev/null").then(|| p.to_string());
        } else if let Some(hunk) = raw.strip_prefix("@@") {
            // "@@ -a,b +c,d @@ ..." — grab the new-side start `c`.
            if let Some(start) = hunk
                .split('+')
                .nth(1)
                .and_then(|s| s.split([',', ' ']).next())
                .and_then(|s| s.parse::<u32>().ok())
            {
                new_line = start;
            }
        } else if let Some(f) = &file {
            match raw.chars().next() {
                Some('+') => {
                    out.entry(f.clone()).or_default().insert(new_line);
                    new_line += 1;
                }
                Some('-') => { /* old-side only; new-side counter unchanged */ }
                // "\ No newline at end of file" is a marker, not a line.
                Some('\\') => {}
                _ => {
                    // context line (leading space, or a blank line in the hunk)
                    out.entry(f.clone()).or_default().insert(new_line);
                    new_line += 1;
                }
            }
        }
    }
    out
}

// --- consensus (pure) --------------------------------------------------------

/// Verifies, clusters, and tags submitted findings against the diff's changed
/// lines. Returns items ranked most-confident first.
pub fn consensus(
    findings: &[StoredFinding],
    changed: &HashMap<String, HashSet<u32>>,
) -> Vec<ConsensusItem> {
    let mut items: Vec<ConsensusItem> = Vec::new();

    // Unverified: cited line isn't a changed line. Each stands alone.
    let (verified, unverified): (Vec<&StoredFinding>, Vec<&StoredFinding>) =
        findings.iter().partition(|sf| {
            changed
                .get(&sf.finding.file)
                .is_some_and(|s| s.contains(&sf.finding.line))
        });

    for sf in unverified {
        items.push(single_item(Verdict::Unverified, sf));
    }

    // Cluster verified findings per file by line proximity.
    let mut by_file: HashMap<&str, Vec<&StoredFinding>> = HashMap::new();
    for sf in verified {
        by_file
            .entry(sf.finding.file.as_str())
            .or_default()
            .push(sf);
    }
    for (_file, mut group) in by_file {
        group.sort_by_key(|sf| sf.finding.line);
        let mut cluster: Vec<&StoredFinding> = Vec::new();
        let mut last_line: Option<u32> = None;
        for sf in group {
            match last_line {
                Some(prev) if sf.finding.line.saturating_sub(prev) > CLUSTER_WINDOW => {
                    items.push(tag_cluster(&cluster));
                    cluster.clear();
                }
                _ => {}
            }
            last_line = Some(sf.finding.line);
            cluster.push(sf);
        }
        if !cluster.is_empty() {
            items.push(tag_cluster(&cluster));
        }
    }

    items.sort_by(|a, b| {
        b.verdict
            .rank()
            .cmp(&a.verdict.rank())
            .then_with(|| b.agents.len().cmp(&a.agents.len()))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line_start.cmp(&b.line_start))
    });
    items
}

fn single_item(verdict: Verdict, sf: &StoredFinding) -> ConsensusItem {
    ConsensusItem {
        verdict,
        file: sf.finding.file.clone(),
        line_start: sf.finding.line,
        line_end: sf.finding.line,
        agents: vec![sf.agent.clone()],
        messages: vec![sf.finding.message.clone()],
        severities: sf.finding.severity.clone().into_iter().collect(),
    }
}

fn tag_cluster(cluster: &[&StoredFinding]) -> ConsensusItem {
    let agents: Vec<String> = dedup_preserve(cluster.iter().map(|sf| sf.agent.clone()));
    let severities: Vec<String> =
        dedup_preserve(cluster.iter().filter_map(|sf| sf.finding.severity.clone()));
    let verdict = if agents.len() >= 2 {
        if severities.len() >= 2 {
            Verdict::Disputed
        } else {
            Verdict::Confirmed
        }
    } else {
        Verdict::Unique
    };
    ConsensusItem {
        verdict,
        file: cluster[0].finding.file.clone(),
        line_start: cluster.iter().map(|sf| sf.finding.line).min().unwrap_or(0),
        line_end: cluster.iter().map(|sf| sf.finding.line).max().unwrap_or(0),
        agents,
        messages: cluster
            .iter()
            .map(|sf| sf.finding.message.clone())
            .collect(),
        severities,
    }
}

fn dedup_preserve(iter: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    iter.filter(|s| seen.insert(s.clone())).collect()
}

/// Renders the ranked items as a markdown review body.
pub fn render_markdown(items: &[ConsensusItem]) -> String {
    if items.is_empty() {
        return "No findings submitted.".to_string();
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for it in items {
        *counts.entry(verdict_label(it.verdict)).or_default() += 1;
    }
    let mut out = String::from("## Consensus review\n\n");
    out.push_str(&format!(
        "{} CONFIRMED · {} DISPUTED · {} UNIQUE · {} UNVERIFIED\n\n",
        counts.get("CONFIRMED").copied().unwrap_or(0),
        counts.get("DISPUTED").copied().unwrap_or(0),
        counts.get("UNIQUE").copied().unwrap_or(0),
        counts.get("UNVERIFIED").copied().unwrap_or(0),
    ));
    for it in items {
        let loc = if it.line_start == it.line_end {
            format!("{}:{}", it.file, it.line_start)
        } else {
            format!("{}:{}-{}", it.file, it.line_start, it.line_end)
        };
        out.push_str(&format!(
            "- **{}** `{}` ({})\n",
            verdict_label(it.verdict),
            loc,
            it.agents.join(", ")
        ));
        for m in &it.messages {
            out.push_str(&format!("  - {m}\n"));
        }
    }
    out
}

fn verdict_label(v: Verdict) -> &'static str {
    match v {
        Verdict::Confirmed => "CONFIRMED",
        Verdict::Disputed => "DISPUTED",
        Verdict::Unique => "UNIQUE",
        Verdict::Unverified => "UNVERIFIED",
    }
}

// --- impure helpers ----------------------------------------------------------

/// The finder's name for attribution — the agent *without* an instance suffix
/// (consensus counts distinct agents, not sessions, so two sessions of one
/// agent shouldn't inflate agreement). `AGENTFLARE_AGENT` → detected host agent
/// → "cli".
pub fn submitter_name() -> String {
    std::env::var("AGENTFLARE_AGENT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(agent_detector::agent_name)
        .unwrap_or_else(|| "cli".to_string())
}

/// Computes a unified diff via git for the local branch. `base`/`head` default
/// to `master`/`HEAD` (three-dot: changes on HEAD since it diverged from base).
pub fn compute_diff(base: Option<&str>, head: Option<&str>) -> Result<String, String> {
    let base = base.unwrap_or("master");
    let head = head.unwrap_or("HEAD");
    let range = format!("{base}...{head}");
    let out = std::process::Command::new("git")
        .args(["diff", "--unified=3", &range])
        .output()
        .map_err(|e| format!("git diff failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git diff {range}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sf(agent: &str, file: &str, line: u32, sev: Option<&str>) -> StoredFinding {
        StoredFinding {
            agent: agent.to_string(),
            finding: Finding {
                file: file.to_string(),
                line,
                message: format!("{agent} at {file}:{line}"),
                severity: sev.map(String::from),
                category: None,
            },
        }
    }

    fn changed(pairs: &[(&str, &[u32])]) -> HashMap<String, HashSet<u32>> {
        pairs
            .iter()
            .map(|(f, lines)| (f.to_string(), lines.iter().copied().collect()))
            .collect()
    }

    #[test]
    fn changed_lines_parses_added_and_context_new_side() {
        let diff = "\
diff --git a/src/foo.rs b/src/foo.rs
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -10,3 +10,4 @@ fn x() {
 ctx a
-old line
+new line one
+new line two
 ctx b
";
        let c = changed_lines(diff);
        let foo = &c["src/foo.rs"];
        // new-side: 10 ctx a, 11 new one, 12 new two, 13 ctx b
        assert!(foo.contains(&10) && foo.contains(&11) && foo.contains(&12) && foo.contains(&13));
    }

    #[test]
    fn changed_lines_does_not_leak_metadata_across_files_in_a_multi_file_diff() {
        // The `diff --git`/`index` lines between file sections must not be
        // counted as context lines of the preceding file (which would insert a
        // phantom changed line and falsely verify a finding citing it).
        let diff = "\
diff --git a/f1 b/f1
--- a/f1
+++ b/f1
@@ -1,1 +1,2 @@
 keep
+added
diff --git a/f2 b/f2
--- a/f2
+++ b/f2
@@ -1,1 +1,1 @@
-old
+new
";
        let c = changed_lines(diff);
        assert_eq!(
            c["f1"],
            [1u32, 2].into_iter().collect::<HashSet<_>>(),
            "f1 should be exactly {{1,2}}, no phantom line 3"
        );
        assert_eq!(c["f2"], [1u32].into_iter().collect::<HashSet<_>>());
    }

    #[test]
    fn changed_lines_ignores_no_newline_marker() {
        let diff = "\
diff --git a/f b/f
--- a/f
+++ b/f
@@ -1,1 +1,1 @@
-old
+new
\\ No newline at end of file
";
        let c = changed_lines(diff);
        assert_eq!(c["f"], [1u32].into_iter().collect::<HashSet<_>>());
    }

    #[test]
    fn finding_off_the_diff_is_unverified() {
        let items = consensus(
            &[sf("a", "src/foo.rs", 99, None)],
            &changed(&[("src/foo.rs", &[10, 11])]),
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].verdict, Verdict::Unverified);
    }

    #[test]
    fn two_agents_same_area_is_confirmed() {
        let ch = changed(&[("src/foo.rs", &[10, 11, 12])]);
        let items = consensus(
            &[
                sf("a", "src/foo.rs", 10, None),
                sf("b", "src/foo.rs", 12, None),
            ],
            &ch,
        );
        assert_eq!(items.len(), 1, "should cluster into one");
        assert_eq!(items[0].verdict, Verdict::Confirmed);
        assert_eq!(items[0].agents.len(), 2);
    }

    #[test]
    fn single_agent_is_unique() {
        let items = consensus(
            &[sf("a", "src/foo.rs", 10, None)],
            &changed(&[("src/foo.rs", &[10])]),
        );
        assert_eq!(items[0].verdict, Verdict::Unique);
    }

    #[test]
    fn same_agent_twice_is_not_confirmed() {
        let ch = changed(&[("src/foo.rs", &[10, 11])]);
        let items = consensus(
            &[
                sf("a", "src/foo.rs", 10, None),
                sf("a", "src/foo.rs", 11, None),
            ],
            &ch,
        );
        assert_eq!(items[0].agents.len(), 1);
        assert_eq!(items[0].verdict, Verdict::Unique);
    }

    #[test]
    fn conflicting_severity_across_agents_is_disputed() {
        let ch = changed(&[("src/foo.rs", &[10, 11])]);
        let items = consensus(
            &[
                sf("a", "src/foo.rs", 10, Some("bug")),
                sf("b", "src/foo.rs", 11, Some("nit")),
            ],
            &ch,
        );
        assert_eq!(items[0].verdict, Verdict::Disputed);
    }

    #[test]
    fn distant_lines_do_not_cluster() {
        let ch = changed(&[("src/foo.rs", &[10, 50])]);
        let items = consensus(
            &[
                sf("a", "src/foo.rs", 10, None),
                sf("b", "src/foo.rs", 50, None),
            ],
            &ch,
        );
        assert_eq!(items.len(), 2, "10 and 50 are far apart → separate");
        assert!(items.iter().all(|i| i.verdict == Verdict::Unique));
    }

    #[test]
    fn confirmed_outranks_unique_outranks_unverified() {
        let ch = changed(&[("f", &[1, 2, 3]), ("g", &[1])]);
        let items = consensus(
            &[
                sf("a", "f", 1, None),
                sf("b", "f", 2, None),   // f:1-2 confirmed
                sf("a", "g", 1, None),   // g:1 unique
                sf("a", "f", 999, None), // unverified
            ],
            &ch,
        );
        assert_eq!(items[0].verdict, Verdict::Confirmed);
        assert_eq!(items.last().unwrap().verdict, Verdict::Unverified);
    }

    #[test]
    fn submit_replaces_prior_findings_from_same_agent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let f1 = Finding {
            file: "a".into(),
            line: 1,
            message: "one".into(),
            severity: None,
            category: None,
        };
        let f2 = Finding {
            file: "a".into(),
            line: 2,
            message: "two".into(),
            severity: None,
            category: None,
        };
        submit(&conn, "o/r", "7", "agentA", std::slice::from_ref(&f1), 100).unwrap();
        submit(&conn, "o/r", "7", "agentA", std::slice::from_ref(&f2), 200).unwrap();
        let loaded = load(&conn, "o/r", "7").unwrap();
        assert_eq!(loaded.len(), 1, "re-submit should replace, not append");
        assert_eq!(loaded[0].finding.line, 2);
    }

    #[test]
    fn record_round_tallies_verified_vs_total_per_agent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let ch = changed(&[("f", &[1, 2])]);
        // agentA: 1 verified (f:1) + 1 off-diff (f:99). agentB: 1 verified.
        let findings = vec![
            sf("agentA", "f", 1, None),
            sf("agentA", "f", 99, None),
            sf("agentB", "f", 2, None),
        ];
        record_round(&conn, "o/r", "7", &findings, &ch, 100).unwrap();
        let s = scores(&conn, Some("o/r")).unwrap();
        let a = s.iter().find(|x| x.agent == "agentA").unwrap();
        assert_eq!((a.findings, a.verified), (2, 1));
        assert!((a.accuracy - 0.5).abs() < 1e-9);
        let b = s.iter().find(|x| x.agent == "agentB").unwrap();
        assert_eq!((b.findings, b.verified), (1, 1));
        assert!((b.accuracy - 1.0).abs() < 1e-9);
        // Ranked by accuracy: B (1.0) before A (0.5).
        assert_eq!(s[0].agent, "agentB");
    }

    #[test]
    fn re_recording_a_round_replaces_not_double_counts() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let ch = changed(&[("f", &[1])]);
        let findings = vec![sf("agentA", "f", 1, None)];
        record_round(&conn, "o/r", "7", &findings, &ch, 100).unwrap();
        record_round(&conn, "o/r", "7", &findings, &ch, 200).unwrap();
        let s = scores(&conn, Some("o/r")).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(
            (s[0].findings, s[0].rounds),
            (1, 1),
            "same round must not double-count"
        );
    }

    #[test]
    fn scores_aggregate_across_distinct_rounds() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let ch = changed(&[("f", &[1])]);
        record_round(&conn, "o/r", "7", &[sf("agentA", "f", 1, None)], &ch, 100).unwrap();
        record_round(&conn, "o/r", "8", &[sf("agentA", "f", 1, None)], &ch, 100).unwrap();
        let s = scores(&conn, Some("o/r")).unwrap();
        assert_eq!((s[0].findings, s[0].rounds), (2, 2));
    }

    #[test]
    fn load_and_clear_are_round_scoped() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let f = Finding {
            file: "a".into(),
            line: 1,
            message: "m".into(),
            severity: None,
            category: None,
        };
        submit(&conn, "o/r", "7", "a", std::slice::from_ref(&f), 100).unwrap();
        submit(&conn, "o/r", "8", "a", std::slice::from_ref(&f), 100).unwrap();
        assert_eq!(clear(&conn, "o/r", "7").unwrap(), 1);
        assert!(load(&conn, "o/r", "7").unwrap().is_empty());
        assert_eq!(load(&conn, "o/r", "8").unwrap().len(), 1);
    }
}
