use super::receipts::{confirm, list, reserve_for_run, unresolved_for_run};
use super::*;

fn database(path: &std::path::Path) -> Connection {
    let c = Connection::open(path).unwrap();
    c.busy_timeout(std::time::Duration::from_secs(10)).unwrap();
    migrate(&c).unwrap();
    c
}

fn enqueue(c: &Connection, to: &str, created_at: i64) -> i64 {
    c.execute(
        "INSERT INTO agent_messages(from_key,to_key,to_address,body,created_at)
         VALUES('audit:sender',?1,?1,'correction',?2)",
        params![to, created_at],
    )
    .unwrap();
    c.last_insert_rowid()
}

#[test]
fn receipt_schema_is_additive_and_idempotent() {
    let c = Connection::open_in_memory().unwrap();
    migrate(&c).unwrap();
    let id = enqueue(&c, "agent:one", 1);
    migrate(&c).unwrap();
    let tables: i64 = c.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='agent_message_receipts'",
        [], |r| r.get(0),
    ).unwrap();
    assert_eq!(tables, 1, "migration must create the receipt ledger");
    assert_eq!(inbox(&c, "agent:one", true, 10).unwrap()[0].id, id);
}

#[test]
fn reservation_excludes_legacy_consumers_and_confirmation_is_bound() {
    let c = database(std::path::Path::new(":memory:"));
    let id = enqueue(&c, "agent:one", 1);
    enqueue(&c, "agent:two", 1);
    enqueue(&c, "agent:one", 100);
    let reserved = reserve_for_run(&c, "run-a", "agent:one", 50, 60).unwrap();
    assert_eq!(reserved.len(), 1);
    let r = &reserved[0];
    assert_eq!(r.message.id, id);
    assert_eq!(r.state, "in_doubt");
    assert!(
        take_undelivered_before(&c, "agent:one", 10, 50, 61)
            .unwrap()
            .is_empty()
    );
    assert_eq!(count_undelivered(&c, "agent:two").unwrap(), 1);
    assert_eq!(
        confirm(&c, &r.receipt_id, "wrong-run", "agent:one", 62).unwrap(),
        0
    );
    assert_eq!(
        confirm(&c, &r.receipt_id, "run-a", "agent:two", 62).unwrap(),
        0
    );
    assert_eq!(
        confirm(&c, &r.receipt_id, "run-a", "agent:one", 62).unwrap(),
        1
    );
    assert_eq!(
        confirm(&c, &r.receipt_id, "run-a", "agent:one", 63).unwrap(),
        0
    );
    let status = list(&c, "agent:one", 10).unwrap();
    assert_eq!(status[0].state, "received");
    assert_eq!(status[0].received_at, Some(62));
    assert!(unresolved_for_run(&c, "run-a").unwrap().is_empty());
}

#[test]
fn reopen_recovers_the_same_reservation_even_after_owner_rebind() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("messages.db");
    let receipt_id = {
        let c = database(&path);
        enqueue(&c, "agent:old", 1);
        reserve_for_run(&c, "run-a", "agent:old", 50, 60).unwrap()[0]
            .receipt_id
            .clone()
    };
    let c = database(&path);
    let resumed = reserve_for_run(&c, "run-a", "agent:new", 50, 70).unwrap();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].receipt_id, receipt_id);
    assert_eq!(resumed[0].message.to_key, "agent:old");
    assert!(
        reserve_for_run(&c, "run-b", "agent:old", 50, 70)
            .unwrap()
            .is_empty()
    );
    assert_eq!(list(&c, "agent:new", 10).unwrap().len(), 0);
}

#[test]
fn failed_receipt_insert_rolls_back_pickup() {
    let c = database(std::path::Path::new(":memory:"));
    enqueue(&c, "agent:one", 1);
    c.execute_batch(
        "CREATE TRIGGER reject_receipt BEFORE INSERT ON agent_message_receipts
        BEGIN SELECT RAISE(ABORT, 'fixture ledger failure'); END;",
    )
    .unwrap();
    assert!(reserve_for_run(&c, "run-a", "agent:one", 50, 60).is_err());
    assert_eq!(count_undelivered(&c, "agent:one").unwrap(), 1);
    assert!(list(&c, "agent:one", 10).unwrap().is_empty());
}

#[test]
fn read_and_requeue_do_not_manufacture_or_reopen_custody() {
    let c = database(std::path::Path::new(":memory:"));
    let reserved_id = enqueue(&c, "agent:one", 1);
    let r = reserve_for_run(&c, "run-a", "agent:one", 50, 60)
        .unwrap()
        .remove(0);
    assert_eq!(requeue(&c, &[reserved_id]).unwrap(), 0);
    assert_eq!(mark_read(&c, "agent:one", &[reserved_id], 61).unwrap(), 1);
    assert_eq!(list(&c, "agent:one", 10).unwrap()[0].state, "in_doubt");
    assert_eq!(
        confirm(&c, &r.receipt_id, "run-a", "agent:one", 62).unwrap(),
        1
    );
    assert_eq!(requeue(&c, &[reserved_id]).unwrap(), 0);
    let legacy = enqueue(&c, "agent:one", 1);
    take_undelivered(&c, "agent:one", 10, 63).unwrap();
    assert_eq!(requeue(&c, &[legacy]).unwrap(), 1);
}

#[test]
fn prune_retains_unknown_receipts_and_cleans_resolved_receipts() {
    let c = database(std::path::Path::new(":memory:"));
    let unknown = enqueue(&c, "agent:one", 1);
    let r = reserve_for_run(&c, "run-a", "agent:one", 50, 60)
        .unwrap()
        .remove(0);
    assert_eq!(prune(&c, 10_000_000).unwrap(), 0);
    assert_eq!(inbox(&c, "agent:one", false, 10).unwrap()[0].id, unknown);
    assert_eq!(list(&c, "agent:one", 10).unwrap().len(), 1);
    confirm(&c, &r.receipt_id, "run-a", "agent:one", 62).unwrap();
    assert_eq!(prune(&c, 10_000_000).unwrap(), 1);
    assert!(list(&c, "agent:one", 10).unwrap().is_empty());
}

#[test]
fn receipt_and_legacy_takers_share_atomic_exclusion() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("messages.db");
    let c = database(&path);
    for _ in 0..40 {
        enqueue(&c, "agent:one", 1);
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let workers: Vec<_> = (0..4)
        .map(|i| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let c = database(&path);
                barrier.wait();
                if i % 2 == 0 {
                    reserve_for_run(&c, &format!("run-{i}"), "agent:one", 50, 60)
                        .unwrap()
                        .into_iter()
                        .map(|r| r.message.id)
                        .collect::<Vec<_>>()
                } else {
                    take_undelivered(&c, "agent:one", MAX_BATCH, 60)
                        .unwrap()
                        .into_iter()
                        .map(|m| m.id)
                        .collect()
                }
            })
        })
        .collect();
    let ids: Vec<_> = workers
        .into_iter()
        .flat_map(|w| w.join().unwrap())
        .collect();
    assert_eq!(ids.len(), 40);
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        40
    );
}
