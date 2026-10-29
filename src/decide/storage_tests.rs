use super::*;
use std::time::Instant;

fn row() -> shadow::Row {
    shadow::Row::answered(
        "bench",
        "never stored",
        "a",
        "b",
        Some(0.9),
        10,
        Some(0.0001),
    )
}

#[test]
fn ledger_enqueue_p95_and_growth_per_thousand() {
    use super::super::{Answer, Question, Usage, tracking};
    use std::collections::BTreeMap;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shadow.jsonl");
    let w = Writer::new(path.clone(), dir.path().join("dataset.jsonl")).unwrap();
    let questions = BTreeMap::from([("route".into(), Question::noul("Private question"))]);
    let answers = BTreeMap::from([("route".into(), Answer::Noul { noul: 0.9 })]);
    let mut micros = Vec::new();
    for i in 0..1000 {
        let start = Instant::now();
        tracking::finish(
            row(),
            tracking::Context {
                questions: &questions,
                answers: &answers,
                source_model: Some("jev-v1"),
                usage: &Usage::default(),
                applied: true,
            },
            None,
            &|row| w.enqueue(Work::Decision(Box::new(row.clone()))),
        );
        micros.push(start.elapsed().as_micros());
        if i % 16 == 15 {
            assert!(w.flush(Duration::from_secs(5)));
        }
    }
    assert!(w.flush(Duration::from_secs(5)));
    micros.sort_unstable();
    assert_eq!(shadow::load(&path).len(), 1000);
    assert_eq!(w.pending_drops(), 0);
    eprintln!(
        "ledger record p95={} us; bytes/1k={}",
        micros[949],
        bytes(&path)
    );
    assert!(micros[949] < 2000);
    assert!(bytes(&path) < 1_000_000);
}

#[test]
fn cold_writer_start_and_enqueue_p95() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shadow.jsonl");
    let mut micros = Vec::new();
    for _ in 0..100 {
        let row = row();
        let start = Instant::now();
        let w = Writer::new(path.clone(), dir.path().join("dataset.jsonl")).unwrap();
        w.enqueue(Work::Decision(Box::new(row)));
        micros.push(start.elapsed().as_micros());
        assert!(w.flush(Duration::from_secs(5)));
    }
    micros.sort_unstable();
    eprintln!(
        "cold writer start + enqueue p50={} us; p95={} us",
        micros[49], micros[94]
    );
    assert!(micros[94] < 2000);
    assert_eq!(shadow::load(&path).len(), 100);
}

#[test]
fn failed_writes_are_counted_and_reported_after_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("blocked");
    std::fs::write(&parent, "file in place of directory").unwrap();
    let path = parent.join("shadow.jsonl");
    let w = Writer::new(path.clone(), parent.join("dataset.jsonl")).unwrap();
    w.enqueue(Work::Decision(Box::new(row())));
    assert!(w.flush(Duration::from_secs(5)));
    assert_eq!(w.pending_drops(), 1);
    std::fs::remove_file(&parent).unwrap();
    w.enqueue(Work::Decision(Box::new(row())));
    assert!(w.flush(Duration::from_secs(5)));
    assert_eq!(w.pending_drops(), 0);
    let report = super::super::report::load(&path, &parent.join("dataset.jsonl"), None);
    assert_eq!(report.dropped_writes, 1);
    assert_eq!(report.all.decisions, 1);
}

#[test]
fn contention_and_rotation_never_wait_or_exceed_two_caps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shadow.jsonl");
    append(&path, &row(), 1500).unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&file).unwrap();
    let start = Instant::now();
    assert!(append(&path, &row(), 1500).is_err());
    assert!(start.elapsed() < Duration::from_millis(100));
    drop(file);
    for _ in 0..20 {
        append(&path, &row(), 1500).unwrap();
    }
    assert!(path.with_extension("jsonl.1").exists());
    assert!(bytes(&path) <= 3000);
    assert!(!shadow::load(&path).is_empty());
}

#[test]
fn queued_batch_rotates_under_one_lock_and_keeps_each_line_readable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shadow.jsonl");
    let rows: Vec<_> = (0..20).map(|_| row()).collect();
    let mut written = Vec::new();
    capture::append_many(
        &path,
        rows.iter().enumerate().map(|(i, row)| (i as u64, row)),
        1500,
        |_| true,
        |ticket| written.push(ticket),
        false,
    )
    .unwrap();
    assert_eq!(written, (0..20).collect::<Vec<_>>());
    assert!(path.with_extension("jsonl.1").exists());
    assert!(bytes(&path) <= 3000);
    let retained = shadow::load(&path);
    assert!(retained.len() > 1);
    assert_eq!(
        retained.last().unwrap().decision_id,
        rows.last().unwrap().decision_id
    );
}

#[test]
fn appending_preserves_a_complete_legacy_row_without_a_trailing_newline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shadow.jsonl");
    let mut old = row();
    old.decision_id = None;
    std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    append(&path, &row(), LEDGER_CAP).unwrap();
    let rows = shadow::load(&path);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].decision_id.is_none());
}

#[test]
fn backpressure_is_accounted_after_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shadow.jsonl");
    let w = Writer::constrained(path.clone(), dir.path().join("dataset.jsonl"), 20).unwrap();
    for _ in 0..100 {
        let mut row = row();
        row.jev = Some("a".repeat(50_000));
        let start = Instant::now();
        w.enqueue(Work::Decision(Box::new(row)));
        assert!(start.elapsed() < Duration::from_millis(100));
    }
    w.shutdown();
    drop(w);
    let report = super::super::report::load(&path, &dir.path().join("dataset.jsonl"), None);
    assert!(report.dropped_writes > 0);
    assert_eq!(report.all.decisions as u64 + report.dropped_writes, 100);
}

#[test]
fn drop_replay_resolves_late_rows_and_deduplicates_identified_notifications() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shadow.jsonl");
    let dataset = dir.path().join("dataset.jsonl");
    let mut decision = row();
    decision.decision_id = Some("late".into());
    append(&path, &decision, LEDGER_CAP).unwrap();
    let mut supplement = capture::build_row(capture::Input {
        site: "bench",
        features: serde_json::json!({}),
        norm_input: "input",
        label: serde_json::json!({}),
        confidence: None,
        baseline: "a",
        source_model: None,
    });
    supplement.decision_id = Some("late".into());
    append(&dataset, &supplement, LEDGER_CAP).unwrap();
    for count in [5, 4] {
        append(
            &path,
            &serde_json::json!({"record_type":"dropped", "count":count,
            "decision_ids":["late", "missing-decision"],
            "capture_ids":["late", "missing-capture"]}),
            LEDGER_CAP,
        )
        .unwrap();
    }
    append(
        &path,
        &serde_json::json!({"record_type":"dropped", "count":2}),
        LEDGER_CAP,
    )
    .unwrap();
    let report = super::super::report::load(&path, &dataset, None);
    assert_eq!(report.all.decisions, 1);
    assert_eq!(report.dropped_writes, 5); // two missing IDs, one anonymous write, two legacy drops
}
