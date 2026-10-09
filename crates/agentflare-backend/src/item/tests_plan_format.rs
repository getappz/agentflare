use super::plan_format::{MAX_TASKS, parse_plan};

fn task(no: usize, title: &str, meta: &str) -> String {
    format!("### Task {no}: {title}\n\nbody {no}\n\n```task\n{meta}\n```\n\n")
}

fn plan(tasks: &[String]) -> String {
    format!("# Plan\n\nGoal: ship it\n\n{}", tasks.concat())
}

#[test]
fn parses_a_valid_plan_with_header_body_and_metadata() {
    let md = plan(&[
        task(1, "Schema", "size: S"),
        task(
            2,
            "Parser",
            "size: M  # medium\ndepends_on: [1]\nmodel_tier: mechanical\nfiles: [a.rs, b.rs]",
        ),
    ]);
    let parsed = parse_plan(&md).unwrap();
    assert!(parsed.header.contains("Goal: ship it"));
    assert_eq!(parsed.tasks.len(), 2);
    let t2 = &parsed.tasks[1];
    assert_eq!(t2.no, 2);
    assert_eq!(t2.title, "Parser");
    assert_eq!(t2.size, "M");
    assert_eq!(t2.body, "body 2");
    assert_eq!(t2.depends_on.iter().copied().collect::<Vec<_>>(), vec![1]);
    assert_eq!(t2.model_tier.as_deref(), Some("mechanical"));
    assert_eq!(t2.files, vec!["a.rs".to_string(), "b.rs".to_string()]);
}

#[test]
fn missing_and_invalid_size_are_rejected_with_the_task_named() {
    let errs = parse_plan(&plan(&[task(1, "A", "parallel: true")])).unwrap_err();
    assert!(
        errs.iter()
            .any(|e| e.contains("task 1") && e.contains("size")),
        "{errs:?}"
    );
    let errs = parse_plan(&plan(&[task(1, "A", "size: m")])).unwrap_err();
    assert!(
        errs.iter()
            .any(|e| e.contains("task 1") && e.contains("S, M or L")),
        "{errs:?}"
    );
}

#[test]
fn dangling_and_self_references_are_rejected() {
    let errs = parse_plan(&plan(&[task(1, "A", "size: S\ndepends_on: [9]")])).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("depends_on 9")), "{errs:?}");
    let errs = parse_plan(&plan(&[task(1, "A", "size: S\ndepends_on: [1]")])).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("depends_on 1")), "{errs:?}");
}

#[test]
fn dependency_cycle_is_rejected() {
    let md = plan(&[
        task(1, "A", "size: S\ndepends_on: [2]"),
        task(2, "B", "size: S\ndepends_on: [1]"),
    ]);
    let errs = parse_plan(&md).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("cycle")), "{errs:?}");
}

#[test]
fn conflict_pair_orders_higher_after_lower_and_is_symmetric() {
    // Declared on the LOWER task only; the higher still gets the edge.
    let md = plan(&[
        task(1, "A", "size: S\nconflicts_with: [2]"),
        task(2, "B", "size: S"),
    ]);
    let parsed = parse_plan(&md).unwrap();
    assert!(parsed.tasks[0].depends_on.is_empty());
    assert!(parsed.tasks[1].depends_on.contains(&1));
}

#[test]
fn conflict_that_contradicts_a_dependency_is_a_cycle() {
    let md = plan(&[
        task(1, "A", "size: S\ndepends_on: [2]\nconflicts_with: [2]"),
        task(2, "B", "size: S"),
    ]);
    let errs = parse_plan(&md).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("cycle")), "{errs:?}");
}

#[test]
fn overlapping_files_imply_a_conflict() {
    let md = plan(&[
        task(1, "A", "size: S\nfiles: [shared.rs]"),
        task(2, "B", "size: S\nfiles: [shared.rs, other.rs]"),
        task(3, "C", "size: S\nfiles: [elsewhere.rs]"),
    ]);
    let parsed = parse_plan(&md).unwrap();
    assert!(parsed.tasks[1].depends_on.contains(&1));
    assert!(parsed.tasks[2].depends_on.is_empty());
}

#[test]
fn implied_file_conflict_defers_to_an_existing_reverse_order() {
    // The planner already ordered 2 before 1; the shared file must not turn
    // that into a cycle (only an explicit contradiction is an error).
    let md = plan(&[
        task(1, "A", "size: S\ndepends_on: [2]\nfiles: [mod.rs]"),
        task(2, "B", "size: S\nfiles: [mod.rs]"),
    ]);
    let parsed = parse_plan(&md).unwrap();
    assert_eq!(
        parsed.tasks[0]
            .depends_on
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![2]
    );
    assert!(parsed.tasks[1].depends_on.is_empty());
}

#[test]
fn implied_file_conflict_defers_to_a_transitive_order() {
    let md = plan(&[
        task(1, "A", "size: S\nfiles: [mod.rs]"),
        task(2, "B", "size: S\ndepends_on: [1]"),
        task(3, "C", "size: S\ndepends_on: [2]\nfiles: [mod.rs]"),
    ]);
    let parsed = parse_plan(&md).unwrap();
    assert_eq!(
        parsed.tasks[2]
            .depends_on
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![2],
        "3 already waits on 1 through 2, so no extra edge"
    );
}

#[test]
fn more_than_the_task_cap_is_rejected() {
    let tasks: Vec<String> = (1..=MAX_TASKS + 1)
        .map(|n| task(n, "T", "size: S"))
        .collect();
    let errs = parse_plan(&plan(&tasks)).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("cap")), "{errs:?}");
}

#[test]
fn empty_plan_unknown_key_and_duplicate_task_are_rejected() {
    assert!(
        parse_plan("# just prose")
            .unwrap_err()
            .iter()
            .any(|e| e.contains("no `### Task"))
    );
    let errs = parse_plan(&plan(&[task(1, "A", "size: S\nbogus: 1")])).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("bogus")), "{errs:?}");
    let errs = parse_plan(&plan(&[task(1, "A", "size: S"), task(1, "B", "size: S")])).unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("declared twice")),
        "{errs:?}"
    );
}
