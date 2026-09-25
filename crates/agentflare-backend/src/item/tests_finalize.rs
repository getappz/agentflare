use super::*;

fn set_state_group(conn: &Connection, item_id: &str, pid: &str, group: &str) {
    update_state(conn, item_id, &state_in_group(conn, pid, group)).unwrap();
}

fn set_metadata(conn: &Connection, item_id: &str, metadata: &str) {
    update(
        conn,
        item_id,
        UpdateItem {
            metadata: Some(metadata.into()),
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn mark_in_review_does_not_revive_a_cancelled_item() {
    // `item_cancel` only releases the canceller's own lease, so a job that
    // still holds its claim can reach its finalize after the cancel landed.
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    set_state_group(&conn, &item.id, &pid, "cancelled");

    assert!(!mark_in_review(&conn, &item.id, "agent:1").unwrap());
    assert!(!mark_completed(&conn, &item.id, "agent:1").unwrap());
    assert_eq!(
        get(&conn, &item.id).unwrap().state_id,
        state_in_group(&conn, &pid, "cancelled")
    );
}

#[test]
fn mark_in_review_does_not_reopen_a_completed_item() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    assert!(mark_completed(&conn, &item.id, "agent:1").unwrap());

    // Lease still held (deferred release), but the item is already done.
    assert!(!mark_in_review(&conn, &item.id, "agent:1").unwrap());
    assert!(!mark_completed(&conn, &item.id, "agent:1").unwrap());
    assert_eq!(
        get(&conn, &item.id).unwrap().state_id,
        state_in_group(&conn, &pid, "completed")
    );
}

#[test]
fn late_finalize_does_not_undo_a_redispatch() {
    // `redispatch` leaves the ledger alone on purpose, so the old job's
    // lease is still "ours" when it finishes -- it must not drag the
    // freshly re-armed backlog item back out.
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "claude-code:1", 1000, TTL).unwrap();
    redispatch(&conn, &item.id, None).unwrap();

    assert!(!mark_completed(&conn, &item.id, "claude-code:1").unwrap());
    assert!(!mark_in_review(&conn, &item.id, "claude-code:1").unwrap());
    assert_eq!(
        get(&conn, &item.id).unwrap().state_id,
        state_in_group(&conn, &pid, "backlog")
    );
}

#[test]
fn mark_completed_from_in_review_still_completes() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    assert!(mark_in_review(&conn, &item.id, "agent:1").unwrap());
    // Idempotent re-finalize into the same group is fine.
    assert!(mark_in_review(&conn, &item.id, "agent:1").unwrap());
    assert!(mark_completed(&conn, &item.id, "agent:1").unwrap());
    assert_eq!(
        get(&conn, &item.id).unwrap().state_id,
        state_in_group(&conn, &pid, "completed")
    );
}

#[test]
fn promote_if_pr_only_promotes_for_the_pr_that_was_checked() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    set_metadata(&conn, &item.id, r#"{"pr":{"number":42,"branch":"b"}}"#);
    assert!(mark_in_review(&conn, &item.id, "agent:1").unwrap());

    // A different (newer) PR is tracked now, or none at all -- refuse.
    assert!(!promote_in_review_to_completed_if_pr(&conn, &item.id, Some(41)).unwrap());
    assert!(!promote_in_review_to_completed_if_pr(&conn, &item.id, None).unwrap());
    assert_eq!(
        get(&conn, &item.id).unwrap().state_id,
        state_in_group(&conn, &pid, "in_review")
    );
    assert!(crate::claim::is_owner(&conn, &item.id, "agent:1").unwrap());

    assert!(promote_in_review_to_completed_if_pr(&conn, &item.id, Some(42)).unwrap());
    assert_eq!(
        get(&conn, &item.id).unwrap().state_id,
        state_in_group(&conn, &pid, "completed")
    );
    assert!(crate::claim::current_owner(&conn, &item.id).is_none());
}

#[test]
fn promote_if_pr_with_no_tracked_pr_matches_none() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    assert!(mark_in_review(&conn, &item.id, "agent:1").unwrap());

    assert!(!promote_in_review_to_completed_if_pr(&conn, &item.id, Some(7)).unwrap());
    assert!(promote_in_review_to_completed_if_pr(&conn, &item.id, None).unwrap());
}

/// Every read-then-write here used to open a DEFERRED transaction: under WAL
/// with other writers committing between its first read and its first
/// write, that fails instantly with SQLITE_BUSY_SNAPSHOT ("database is
/// locked") instead of waiting on busy_timeout. Hammer the finalize paths
/// from several connections at once; with the write lock taken upfront none
/// of them may error.
#[test]
fn finalize_paths_survive_concurrent_writers_on_a_shared_file_db() {
    const THREADS: usize = 8;
    const ROUNDS: i64 = 15;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("backend.db");
    let item_ids: Vec<String> = {
        let conn = db::open_db(&path).unwrap();
        let (pid, sid) = seed_project(&conn, "");
        (0..THREADS)
            .map(|_| make_item(&conn, &pid, &sid).id)
            .collect()
    };

    let handles: Vec<_> = item_ids
        .into_iter()
        .enumerate()
        .map(|(t, item_id)| {
            let path = path.clone();
            std::thread::spawn(move || -> std::result::Result<(), String> {
                let conn = db::open_db(&path).map_err(|e| e.to_string())?;
                let owner = format!("claude-code:{t}");
                for round in 0..ROUNDS {
                    let now = 1_000 + round * 10;
                    let step = |what: &str, r: Result<bool>| match r {
                        Ok(true) => Ok(()),
                        Ok(false) => Err(format!("thread {t} round {round}: {what} was refused")),
                        Err(e) => Err(format!("thread {t} round {round}: {what}: {e}")),
                    };
                    match claim(&conn, &item_id, &owner, now, TTL) {
                        Ok(ClaimOutcome::Acquired) => {}
                        other => return Err(format!("thread {t} round {round}: claim {other:?}")),
                    }
                    redispatch(&conn, &item_id, None)
                        .map_err(|e| format!("thread {t} round {round}: redispatch: {e}"))?;
                    match claim(&conn, &item_id, &owner, now + 1, TTL) {
                        Ok(ClaimOutcome::Acquired) => {}
                        other => {
                            return Err(format!("thread {t} round {round}: reclaim {other:?}"));
                        }
                    }
                    if round % 2 == 0 {
                        step("mark_in_review", mark_in_review(&conn, &item_id, &owner))?;
                        step("promote", promote_in_review_to_completed(&conn, &item_id))?;
                    } else {
                        step("mark_completed", mark_completed(&conn, &item_id, &owner))?;
                        step("release", release(&conn, &item_id, &owner))?;
                    }
                }
                Ok(())
            })
        })
        .collect();

    let errors: Vec<String> = handles
        .into_iter()
        .filter_map(|h| h.join().unwrap().err())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn forced_completion_promotes_from_backlog_but_never_revives_a_cancelled_item() {
    // The audited force path (merged PR, `done` never ran) may complete an
    // item still in backlog; it must still not revive a cancelled one.
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    crate::claim::acquire(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    set_state_group(&conn, &item.id, &pid, "backlog");
    assert!(!mark_completed(&conn, &item.id, "agent:1").unwrap());
    assert!(mark_completed_forced(&conn, &item.id, "agent:1").unwrap());
    assert_eq!(
        get(&conn, &item.id).unwrap().state_id,
        state_in_group(&conn, &pid, "completed")
    );

    let other = make_item(&conn, &pid, &sid);
    crate::claim::acquire(&conn, &other.id, "agent:1", 1000, TTL).unwrap();
    set_state_group(&conn, &other.id, &pid, "cancelled");
    assert!(!mark_completed_forced(&conn, &other.id, "agent:1").unwrap());
}

#[test]
fn forced_completion_if_pr_only_completes_for_the_pr_that_was_checked() {
    // A redispatch + fresh PR between the merge check and the promote must
    // not complete the item off the old attempt's merge.
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    crate::claim::acquire(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    set_state_group(&conn, &item.id, &pid, "backlog");
    set_metadata(&conn, &item.id, r#"{"pr":{"number":43,"branch":"b"}}"#);

    assert!(!mark_completed_forced_if_pr(&conn, &item.id, "agent:1", Some(42)).unwrap());
    assert!(!mark_completed_forced_if_pr(&conn, &item.id, "agent:1", None).unwrap());
    assert_eq!(
        get(&conn, &item.id).unwrap().state_id,
        state_in_group(&conn, &pid, "backlog")
    );

    assert!(mark_completed_forced_if_pr(&conn, &item.id, "agent:1", Some(43)).unwrap());
    assert_eq!(
        get(&conn, &item.id).unwrap().state_id,
        state_in_group(&conn, &pid, "completed")
    );

    let untracked = make_item(&conn, &pid, &sid);
    crate::claim::acquire(&conn, &untracked.id, "agent:1", 1000, TTL).unwrap();
    assert!(!mark_completed_forced_if_pr(&conn, &untracked.id, "agent:1", Some(7)).unwrap());
    assert!(mark_completed_forced_if_pr(&conn, &untracked.id, "agent:1", None).unwrap());
}
