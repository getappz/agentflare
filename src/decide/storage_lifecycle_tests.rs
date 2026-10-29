use super::*;
use std::{process::Command, time::Instant};

fn child(root: &Path, scenario: &str) -> std::process::Output {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "decide::storage::lifecycle_tests::exit_child",
            "--ignored",
            "--nocapture",
        ])
        .env("AGENTFLARE_HOME_OVERRIDE", root)
        .env("AGENTFLARE_TEST_LEDGER_SCENARIO", scenario)
        .env(
            "AGENTFLARE_TEST_LEDGER_START_FAILURE",
            if scenario.starts_with("unavailable") {
                "1"
            } else {
                "0"
            },
        )
        .env(
            "AGENTFLARE_TEST_LEDGER_LOCK_DELAY",
            if scenario == "locked" { "1" } else { "0" },
        )
        .output()
        .unwrap()
}

fn settled(root: &Path, writes: u64) -> super::super::report::Report {
    let root = root.join(".agentflare/decide");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let report = super::super::report::load(
            &root.join("shadow.jsonl"),
            &root.join("dataset.jsonl"),
            None,
        );
        let accounted = report.all.decisions as u64
            + capture::load(&root.join("dataset.jsonl")).len() as u64
            + report.dropped_writes;
        if accounted == writes {
            return report;
        }
        assert!(
            Instant::now() < deadline,
            "expected {writes} writes, accounted for {accounted}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "subprocess entrypoint"]
fn recovery_child() {
    if std::env::var_os(super::super::recovery::ACTIVE).is_none() {
        return;
    }
    assert!(Writer::new("unused-ledger".into(), "unused-dataset".into()).is_err());
    super::super::recovery::run_from_env().unwrap();
}

#[test]
#[ignore = "subprocess entrypoint"]
fn exit_child() {
    let Ok(scenario) = std::env::var("AGENTFLARE_TEST_LEDGER_SCENARIO") else {
        return;
    };
    if scenario == "disabled" {
        let tracked = super::super::tracking::ask_with(
            "disabled",
            "a",
            |_| false,
            &serde_json::Value::Null,
            &Default::default(),
            || Err(super::super::DecideError::Disabled),
        );
        assert!(tracked.decision_id.is_none());
        assert!(WRITER.get().is_none());
        super::exit(0);
    }
    let path = shadow::log_path();
    let dataset = capture::dataset_path();
    if scenario == "partial-tail" {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{\"ts\":").unwrap();
        std::fs::write(&dataset, b"{\"site\":").unwrap();
    } else if scenario == "blocked-capture" {
        std::fs::create_dir_all(&dataset).unwrap();
    }
    if matches!(
        scenario.as_str(),
        "timeout" | "large-timeout" | "partial-tail" | "blocked-capture"
    ) {
        assert!(
            WRITER
                .set(Some(Writer::delayed(path, dataset, 200).unwrap()))
                .is_ok()
        );
    }
    let start = Instant::now();
    let n = if scenario == "benchmark" { 1 } else { 8 };
    for _ in 0..n {
        let mut row = shadow::Row::answered("exit", "private input", "a", "b", Some(0.9), 1, None);
        if scenario == "large-timeout" {
            row.jev = Some("b".repeat(8000));
        }
        let id = row.decision_id.clone();
        record(row);
        if matches!(
            scenario.as_str(),
            "normal" | "timeout" | "locked" | "partial-tail" | "blocked-capture"
        ) {
            let mut supplement = capture::build_row(capture::Input {
                site: "exit",
                features: serde_json::json!({}),
                norm_input: "input",
                label: serde_json::json!({"summary":"b"}),
                confidence: Some(0.9),
                baseline: "a",
                source_model: None,
            });
            supplement.decision_id = id;
            capture(supplement);
        }
    }
    if scenario == "unavailable-flush" {
        assert!(flush(Duration::from_millis(50)));
        assert_eq!(pending_drops(), 0);
    }
    if scenario == "benchmark" {
        println!("COLD_RECORD_US={}", start.elapsed().as_micros());
        let exit_start = Instant::now();
        shutdown();
        println!("SHUTDOWN_US={}", exit_start.elapsed().as_micros());
    }
    super::exit(0);
}

#[test]
fn real_process_exit_preserves_all_decisions_and_capture_joins() {
    let dir = tempfile::tempdir().unwrap();
    let output = child(dir.path(), "normal");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = dir.path().join(".agentflare/decide/shadow.jsonl");
    let dataset = dir.path().join(".agentflare/decide/dataset.jsonl");
    let report = settled(dir.path(), 16);
    let rows = shadow::load(&path);
    let captured = capture::load(&dataset);
    assert_eq!(rows.len(), 8);
    assert_eq!(captured.len(), 8);
    assert!(
        captured
            .iter()
            .all(|c| rows.iter().any(|r| r.decision_id == c.decision_id))
    );
    assert_eq!(report.dropped_writes, 0);
}

#[test]
fn failed_startup_drops_survive_flush_and_real_process_exit() {
    for scenario in ["unavailable-exit", "unavailable-flush"] {
        let dir = tempfile::tempdir().unwrap();
        let output = child(dir.path(), scenario);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = settled(dir.path(), 8);
        assert_eq!(report.all.decisions, 0);
        assert_eq!(report.dropped_writes, 8);
    }
}

#[test]
fn disabled_exit_creates_no_storage_or_writer() {
    let dir = tempfile::tempdir().unwrap();
    let output = child(dir.path(), "disabled");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!dir.path().join(".agentflare").exists());
}

#[test]
fn timed_out_exit_persists_both_decision_and_capture_drops() {
    let dir = tempfile::tempdir().unwrap();
    let output = child(dir.path(), "timeout");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let root = dir.path().join(".agentflare/decide");
    let report = settled(dir.path(), 16);
    assert_eq!(
        report.all.decisions as u64
            + capture::load(&root.join("dataset.jsonl")).len() as u64
            + report.dropped_writes,
        16
    );
    assert_eq!(report.all.decisions, 8);
    assert_eq!(report.dropped_writes, 0);
}

#[test]
fn locked_inflight_write_recovers_after_real_process_exit() {
    let dir = tempfile::tempdir().unwrap();
    let output = child(dir.path(), "locked");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = settled(dir.path(), 16);
    assert_eq!(report.all.decisions, 8);
    assert_eq!(report.dropped_writes, 0);
}

#[test]
fn oversized_recovery_payload_keeps_loss_counts_after_exit() {
    let dir = tempfile::tempdir().unwrap();
    let output = child(dir.path(), "large-timeout");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = settled(dir.path(), 8);
    assert!(report.dropped_writes > 0);
    assert_eq!(report.all.decisions as u64 + report.dropped_writes, 8);
}

#[test]
fn capture_recovery_failure_still_persists_ledger_accounting() {
    let dir = tempfile::tempdir().unwrap();
    let output = child(dir.path(), "blocked-capture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = settled(dir.path(), 16);
    assert_eq!(report.all.decisions, 8);
    assert_eq!(report.dropped_writes, 8);
}

#[test]
fn recovery_repairs_interrupted_json_tails_before_restoring_rows() {
    let dir = tempfile::tempdir().unwrap();
    let output = child(dir.path(), "partial-tail");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = settled(dir.path(), 16);
    assert_eq!(report.all.decisions, 8);
    assert_eq!(report.dropped_writes, 0);
}

#[test]
fn cold_process_record_and_shutdown_overhead() {
    let dir = tempfile::tempdir().unwrap();
    let (mut record, mut shutdown) = (Vec::new(), Vec::new());
    for i in 0..20 {
        let root = dir.path().join(i.to_string());
        let output = child(&root, "benchmark");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        let metric = |name: &str| {
            text.split(name)
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .parse::<u128>()
                .unwrap()
        };
        record.push(metric("COLD_RECORD_US="));
        shutdown.push(metric("SHUTDOWN_US="));
        assert_eq!(settled(&root, 1).all.decisions, 1);
    }
    record.sort_unstable();
    shutdown.sort_unstable();
    eprintln!(
        "cold process full record p50={} us p95={} us; shutdown p50={} us p95={} us",
        record[9], record[18], shutdown[9], shutdown[18]
    );
}
