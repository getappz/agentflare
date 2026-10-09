//! Parser + validator for the planning workflow's plan markdown. Pure: no DB,
//! no I/O. `decompose` consumes the result.
//!
//! A plan is `writing-plans` markdown: free-form header prose, then
//! `### Task N: <title>` sections. Each task carries one fenced metadata block
//! (a code fence whose info string is `task`) with `key: value` lines:
//! `size` (S|M|L, required), `depends_on`, `parallel`, `conflicts_with`,
//! `model_tier` and `files`.

use std::collections::{BTreeMap, BTreeSet};

pub const MAX_TASKS: usize = 10;
const SIZES: [&str; 3] = ["S", "M", "L"];
const TIERS: [&str; 3] = ["mechanical", "integration", "architecture"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanTask {
    pub no: usize,
    pub title: String,
    pub body: String,
    pub size: String,
    /// Effective prerequisites: declared `depends_on` plus, for every
    /// conflicting pair, an edge from the lower to the higher task number.
    pub depends_on: BTreeSet<usize>,
    pub parallel: bool,
    pub model_tier: Option<String>,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPlan {
    pub header: String,
    pub tasks: Vec<PlanTask>,
}

#[derive(Default)]
struct Meta {
    size: Option<String>,
    depends_on: BTreeSet<usize>,
    conflicts_with: BTreeSet<usize>,
    parallel: bool,
    model_tier: Option<String>,
    files: Vec<String>,
}

/// Parses and validates `markdown`. All problems are collected so the planner
/// can fix them in one revision instead of one error per round trip.
pub fn parse_plan(markdown: &str) -> Result<ParsedPlan, Vec<String>> {
    let mut errors = Vec::new();
    let mut header = String::new();
    let mut raw: Vec<(usize, String, Vec<&str>)> = Vec::new();
    for line in markdown.lines() {
        if let Some(rest) = line.strip_prefix("### Task ") {
            let parsed = rest
                .split_once(':')
                .and_then(|(n, t)| Some((n.trim().parse::<usize>().ok()?, t.trim().to_string())));
            match parsed {
                Some((no, title)) if !title.is_empty() => raw.push((no, title, Vec::new())),
                _ => errors.push(format!("malformed task heading: {line:?}")),
            }
        } else if let Some((_, _, lines)) = raw.last_mut() {
            lines.push(line);
        } else {
            header.push_str(line);
            header.push('\n');
        }
    }
    if raw.is_empty() {
        errors.push("plan has no `### Task N: <title>` headings".to_string());
    }
    if raw.len() > MAX_TASKS {
        errors.push(format!(
            "plan has {} tasks; the cap is {MAX_TASKS} -- split it into sub-epics",
            raw.len()
        ));
    }

    let mut tasks = Vec::new();
    let mut conflicts: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for (no, title, lines) in raw {
        if !seen.insert(no) {
            errors.push(format!("task {no} is declared twice"));
            continue;
        }
        let (meta_lines, body) = split_meta(&lines);
        let meta = parse_meta(no, &meta_lines, &mut errors);
        let size = match meta.size {
            Some(s) if SIZES.contains(&s.as_str()) => s,
            Some(s) => {
                errors.push(format!("task {no}: size {s:?} must be S, M or L"));
                continue;
            }
            None => {
                errors.push(format!("task {no}: `size` is required"));
                continue;
            }
        };
        if let Some(t) = &meta.model_tier
            && !TIERS.contains(&t.as_str())
        {
            errors.push(format!(
                "task {no}: model_tier {t:?} must be mechanical, integration or architecture"
            ));
        }
        conflicts.insert(no, meta.conflicts_with);
        tasks.push(PlanTask {
            no,
            title,
            body,
            size,
            depends_on: meta.depends_on,
            parallel: meta.parallel,
            model_tier: meta.model_tier,
            files: meta.files,
        });
    }

    // Tasks that touch the same file conflict even if the planner forgot to say
    // so. Kept apart from declared conflicts: an implied one only has to
    // serialize the pair, so it yields to any order the planner already gave.
    let mut implied: Vec<(usize, usize)> = Vec::new();
    for (i, a) in tasks.iter().enumerate() {
        for b in &tasks[i + 1..] {
            if a.files.iter().any(|f| b.files.contains(f)) {
                implied.push((a.no.min(b.no), a.no.max(b.no)));
            }
        }
    }

    let known: BTreeSet<usize> = tasks.iter().map(|t| t.no).collect();
    for t in &tasks {
        let declared_conflicts = conflicts.get(&t.no).into_iter().flatten();
        for d in t.depends_on.iter().chain(declared_conflicts) {
            if *d == t.no || !known.contains(d) {
                errors.push(format!(
                    "task {}: depends_on {d} is not another task in this plan",
                    t.no
                ));
            }
        }
    }

    // Conflicts become ordering: the higher number waits for the lower.
    let known_ref = &known;
    let pairs: Vec<(usize, usize)> = conflicts
        .iter()
        .flat_map(|(a, set)| {
            let a = *a;
            set.iter()
                .filter(move |b| known_ref.contains(b) && **b != a)
                .map(move |b| (a.min(*b), a.max(*b)))
        })
        .collect();
    for (lo, hi) in pairs {
        if let Some(t) = tasks.iter_mut().find(|t| t.no == hi) {
            t.depends_on.insert(lo);
        }
    }
    for (lo, hi) in implied {
        let ordered = depends_transitively(&tasks, hi, lo) || depends_transitively(&tasks, lo, hi);
        if !ordered && let Some(t) = tasks.iter_mut().find(|t| t.no == hi) {
            t.depends_on.insert(lo);
        }
    }

    if errors.is_empty()
        && let Some(stuck) = find_cycle(&tasks)
    {
        errors.push(format!("dependency cycle among tasks {stuck:?}"));
    }
    if errors.is_empty() {
        Ok(ParsedPlan { header, tasks })
    } else {
        Err(errors)
    }
}

/// Whether task `from` waits on task `target`, directly or through other tasks.
fn depends_transitively(tasks: &[PlanTask], from: usize, target: usize) -> bool {
    let mut stack = vec![from];
    let mut seen = BTreeSet::new();
    while let Some(n) = stack.pop() {
        if !seen.insert(n) {
            continue;
        }
        if let Some(t) = tasks.iter().find(|t| t.no == n) {
            for d in &t.depends_on {
                if *d == target {
                    return true;
                }
                stack.push(*d);
            }
        }
    }
    false
}

/// Kahn's algorithm; returns the task numbers that can never become ready.
fn find_cycle(tasks: &[PlanTask]) -> Option<Vec<usize>> {
    let mut indegree: BTreeMap<usize, usize> =
        tasks.iter().map(|t| (t.no, t.depends_on.len())).collect();
    let mut ready: Vec<usize> = indegree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| *n)
        .collect();
    let mut done = 0;
    while let Some(n) = ready.pop() {
        done += 1;
        for t in tasks.iter().filter(|t| t.depends_on.contains(&n)) {
            if let Some(d) = indegree.get_mut(&t.no) {
                *d -= 1;
                if *d == 0 {
                    ready.push(t.no);
                }
            }
        }
    }
    (done < tasks.len()).then(|| {
        indegree
            .into_iter()
            .filter(|(_, d)| *d > 0)
            .map(|(n, _)| n)
            .collect()
    })
}

/// Splits a task's lines into (metadata-block lines, remaining body text).
fn split_meta<'a>(lines: &[&'a str]) -> (Vec<&'a str>, String) {
    let (mut meta, mut body, mut in_meta) = (Vec::new(), String::new(), false);
    for line in lines {
        match (in_meta, line.trim()) {
            (false, "```task") => in_meta = true,
            (true, "```") => in_meta = false,
            (true, _) => meta.push(*line),
            (false, _) => {
                body.push_str(line);
                body.push('\n');
            }
        }
    }
    (meta, body.trim().to_string())
}

fn parse_meta(no: usize, lines: &[&str], errors: &mut Vec<String>) -> Meta {
    let mut m = Meta::default();
    for raw in lines {
        let line = raw.split(" #").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            errors.push(format!(
                "task {no}: metadata line {line:?} is not `key: value`"
            ));
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "size" => m.size = Some(value.to_string()),
            "parallel" => match value {
                "true" => m.parallel = true,
                "false" => m.parallel = false,
                _ => errors.push(format!("task {no}: parallel must be true or false")),
            },
            "model_tier" => m.model_tier = Some(value.to_string()),
            "depends_on" => m.depends_on = numbers(no, "depends_on", value, errors),
            "conflicts_with" => m.conflicts_with = numbers(no, "conflicts_with", value, errors),
            "files" => m.files = list(value).into_iter().map(str::to_string).collect(),
            other => errors.push(format!("task {no}: unknown metadata key {other:?}")),
        }
    }
    m
}

fn list(value: &str) -> Vec<&str> {
    value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

fn numbers(no: usize, key: &str, value: &str, errors: &mut Vec<String>) -> BTreeSet<usize> {
    list(value)
        .into_iter()
        .filter_map(|s| match s.parse() {
            Ok(n) => Some(n),
            Err(_) => {
                errors.push(format!("task {no}: {key} entry {s:?} is not a task number"));
                None
            }
        })
        .collect()
}
