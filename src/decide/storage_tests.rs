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
    assert_eq!(w.dropped.load(Ordering::Relaxed), 0);
    eprintln!(
        "ledger record p95={} us; bytes/1k={}",
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
    assert_eq!(w.dropped.load(Ordering::Relaxed), 1);
    std::fs::remove_file(&parent).unwrap();
    w.enqueue(Work::Decision(Box::new(row())));
    assert!(w.flush(Duration::from_secs(5)));
    assert_eq!(w.dropped.load(Ordering::Relaxed), 0);
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
fn saturated_queue_drops_without_waiting() {
    let (tx, _rx) = mpsc::sync_channel(1);
    let w = Writer {
        tx,
        dropped: Arc::new(AtomicU64::new(0)),
    };
    w.enqueue(Work::Decision(Box::new(row())));
    let start = Instant::now();
    w.enqueue(Work::Decision(Box::new(row())));
    assert!(start.elapsed() < Duration::from_millis(100));
    assert_eq!(w.dropped.load(Ordering::Relaxed), 1);
}
