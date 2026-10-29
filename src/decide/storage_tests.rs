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
        assert!(w.receipt(Duration::from_secs(1)));
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
        "ledger record + receipt p95={} us; bytes/1k={}",
        micros[949],
        bytes(&path)
    );
    assert!(micros[949] < 2000);
    assert!(bytes(&path) < 1_000_000);
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
fn backpressure_and_partial_frames_are_accounted_after_shutdown() {
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
#[ignore = "subprocess entrypoint"]
fn writer_child() {
    if let Ok(args) = std::env::var("AGENTFLARE_TEST_WRITER") {
        let (address, token) = args.split_once(' ').unwrap();
        super::super::writer_process::run(address, token).unwrap();
    }
}

#[test]
#[ignore = "subprocess entrypoint"]
fn short_lived_parent() {
    let Ok(root) = std::env::var("AGENTFLARE_TEST_WRITER_PARENT") else {
        return;
    };
    let root = PathBuf::from(root);
    let w = Writer::delayed(root.join("shadow.jsonl"), root.join("dataset.jsonl"), 80).unwrap();
    for _ in 0..8 {
        let row = row();
        let mut capture = super::super::capture::build_row(super::super::capture::Input {
            site: "bench",
            features: serde_json::json!({}),
            norm_input: "state",
            label: serde_json::json!({"summary":"b"}),
            confidence: Some(0.9),
            baseline: "a",
            source_model: None,
        });
        capture.decision_id = row.decision_id.clone();
        w.enqueue(Work::Decision(Box::new(row)));
        w.enqueue(Work::Capture(Box::new(capture)));
    }
    assert!(!w.flush(Duration::from_millis(1)));
    let start = Instant::now();
    w.shutdown();
    assert!(start.elapsed() < Duration::from_millis(50));
    std::process::exit(0);
}

#[test]
fn normal_process_exit_preserves_delayed_decisions_and_capture_joins() {
    let dir = tempfile::tempdir().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "decide::storage::tests::short_lived_parent",
            "--ignored",
        ])
        .env("AGENTFLARE_TEST_WRITER_PARENT", dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    let path = dir.path().join("shadow.jsonl");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let events: Vec<serde_json::Value> = load(&path);
        if events
            .iter()
            .any(|e| e["record_type"] == "test_writer_closed")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "helper did not finish accepted records after caller exit"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let rows = shadow::load(&path);
    let captured = super::super::capture::load(&dir.path().join("dataset.jsonl"));
    assert_eq!(rows.len(), 8);
    assert_eq!(captured.len(), 8);
    assert!(
        captured
            .iter()
            .all(|c| rows.iter().any(|r| r.decision_id == c.decision_id))
    );
    let report = super::super::report::load(&path, &dir.path().join("dataset.jsonl"), None);
    assert_eq!(report.dropped_writes, 0);
}
