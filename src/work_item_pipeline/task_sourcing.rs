/// Decides whether a dispatched item is a no-code task (item #507, widened
/// by #156, given a `task_type` override in #170): `sdd_loop` should
/// analyze/propose rather than implement, and `finalize` should post a
/// findings comment rather than running `item_done`/PR flow.
///
/// `metadata["task_type"]` is the authoritative signal whenever a caller
/// sets it: `"review"`/`"design-spec"`/`"design_spec"` force `true`, and
/// ANY OTHER explicit string forces `false` — skipping the free-text scan
/// below entirely, even if the description contains review-only-sounding
/// prose. This means `task_type` is trusted absolutely once set: it must
/// only ever be set to a value that actually reflects whether the task is
/// no-code, never for an unrelated purpose (e.g. a general category label)
/// on the same field, or a real review-only item could be silently
/// dispatched as an implementation task. Only when `task_type` is
/// absent/non-string does the free-text fallback below run — today that's
/// every caller, since nothing sets `task_type` in production yet, making
/// the fallback load-bearing in practice.
///
/// The fallback matches the "review only" / "design-spec" framing a
/// human/agent handoff uses in prose when no structured field is set —
/// item #502's own handoff read "REVIEW ONLY — do not fix, do not push, do
/// not open a PR." with nothing else marking it as such, and the pipeline
/// implemented it anyway. Hyphens are normalized to spaces before matching
/// because the PM skill's own routing table (`.claude/skills/pm/SKILL.md`)
/// tells callers to write the hyphenated "review-only", which the original
/// space-separated-only check missed (item #156).
pub(crate) fn detect_review_only(item_description: &str, metadata: &serde_json::Value) -> bool {
    match metadata["task_type"].as_str() {
        Some("review") | Some("design-spec") | Some("design_spec") => return true,
        // An explicit, non-forcing task_type (e.g. "implementation") is a
        // caller's deliberate signal that this isn't a review-only task —
        // skip the free-text scan entirely rather than let ordinary prose
        // override it. Only fall through to the scan when task_type is
        // absent/non-string, i.e. no signal was given either way.
        Some(_) => return false,
        None => {}
    }
    let normalized = item_description.to_lowercase().replace('-', " ");
    if normalized.contains("review only") {
        return true;
    }
    // Word-boundary match on "design spec" so "design specification"/"design
    // specs" (ordinary implementation-task phrasing) doesn't false-positive
    // on the "spec" prefix.
    let words: Vec<&str> = normalized.split_whitespace().collect();
    words.windows(2).any(|pair| {
        pair[0].trim_matches(|c: char| !c.is_alphanumeric()) == "design"
            && pair[1].trim_matches(|c: char| !c.is_alphanumeric()) == "spec"
    })
}

/// Whether TDD mode (item #179) is on for this dispatch: a deliberate,
/// item-level opt-in read straight from metadata — no free-text fallback
/// like `detect_review_only` needs, since there are no legacy callers to
/// support for a brand-new flag.
pub(crate) fn detect_tdd_mode(metadata: &serde_json::Value) -> bool {
    metadata["tdd"].as_bool().unwrap_or(false)
}

/// Parses `### Task N: <title>` headings (the convention this codebase's
/// own plans already use — see docs on item #110) into a task list; falls
/// back to a single synthesized task from the item's own description when
/// no plan doc is attached or it contains no recognizable task headings.
pub(crate) fn load_or_synthesize_tasks(
    item_description: &str,
    plan_doc: Option<&str>,
) -> Vec<SddTask> {
    if let Some(doc) = plan_doc.filter(|d| !d.trim().is_empty()) {
        let tasks = parse_task_headings(doc);
        if !tasks.is_empty() {
            return tasks;
        }
    }
    vec![SddTask {
        id: 0,
        title: "Item work".to_string(),
        body: item_description.to_string(),
        model_tier: None,
    }]
}

fn parse_task_headings(doc: &str) -> Vec<SddTask> {
    let mut tasks = Vec::new();
    let mut current: Option<(String, String)> = None;

    for line in doc.lines() {
        if let Some(title) = line.strip_prefix("### Task ").and_then(|rest| {
            let (_num, title) = rest.split_once(':')?;
            Some(title.trim().to_string())
        }) {
            if let Some((title, body)) = current.take() {
                tasks.push(SddTask {
                    id: tasks.len(),
                    title,
                    body: body.trim().to_string(),
                    model_tier: None,
                });
            }
            current = Some((title, String::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some((title, body)) = current {
        tasks.push(SddTask {
            id: tasks.len(),
            title,
            body: body.trim().to_string(),
            model_tier: None,
        });
    }
    tasks
}
