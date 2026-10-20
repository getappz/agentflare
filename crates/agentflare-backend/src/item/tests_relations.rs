use super::*;

#[test]
fn add_relation_blocks_delegates_to_add_dependency() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let a = make_item(&conn, &pid, &sid);
    let b = make_item(&conn, &pid, &sid);

    add_relation(&conn, &b.id, &a.id, "blocks").unwrap();

    assert_eq!(list_dependencies(&conn, &b.id).unwrap(), vec![a.id.clone()]);
    assert_eq!(
        list_relations_by_type(&conn, &b.id, "blocks").unwrap(),
        vec![a.id.clone()]
    );

    remove_relation(&conn, &b.id, &a.id, "blocks").unwrap();
    assert!(list_dependencies(&conn, &b.id).unwrap().is_empty());
}

#[test]
fn symmetric_relation_insertion_order_is_idempotent() {
    // Regression test for item #3 spec §4.2: (A, B, "duplicate") and
    // (B, A, "duplicate") must never both get stored as separate rows --
    // either insertion order must read back identically from either item's
    // perspective.
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let a = make_item(&conn, &pid, &sid);
    let b = make_item(&conn, &pid, &sid);

    add_relation(&conn, &a.id, &b.id, "duplicate").unwrap();
    add_relation(&conn, &b.id, &a.id, "duplicate").unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM item_dependencies WHERE relation_type = 'duplicate'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "both insertion orders must canonicalize to the same row"
    );

    assert_eq!(
        list_relations_by_type(&conn, &a.id, "duplicate").unwrap(),
        vec![b.id.clone()]
    );
    assert_eq!(
        list_relations_by_type(&conn, &b.id, "duplicate").unwrap(),
        vec![a.id.clone()]
    );
}

#[test]
fn remove_relation_symmetric_type_removes_regardless_of_argument_order() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let a = make_item(&conn, &pid, &sid);
    let b = make_item(&conn, &pid, &sid);

    add_relation(&conn, &a.id, &b.id, "relates_to").unwrap();
    remove_relation(&conn, &b.id, &a.id, "relates_to").unwrap();

    assert!(
        list_relations_by_type(&conn, &a.id, "relates_to")
            .unwrap()
            .is_empty()
    );
    assert!(
        list_relations_by_type(&conn, &b.id, "relates_to")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn list_all_relations_returns_pairs_across_all_types() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let a = make_item(&conn, &pid, &sid);
    let b = make_item(&conn, &pid, &sid);
    let c = make_item(&conn, &pid, &sid);

    add_relation(&conn, &a.id, &b.id, "blocks").unwrap();
    add_relation(&conn, &a.id, &c.id, "duplicate").unwrap();

    let mut relations = list_all_relations(&conn, &a.id).unwrap();
    relations.sort();
    let mut expected = vec![
        ("blocks".to_string(), b.id.clone()),
        ("duplicate".to_string(), c.id.clone()),
    ];
    expected.sort();
    assert_eq!(relations, expected);
}

/// Regression test for item #3 spec §3: a `duplicate` relation coexisting
/// with a `blocks` relation between the same pair must not change
/// `all_dependencies_completed`'s (or any other `blocks`-only reader's)
/// behavior.
#[test]
fn duplicate_relation_does_not_affect_all_dependencies_completed() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let blocker = make_item(&conn, &pid, &sid);
    let dependent = make_item(&conn, &pid, &sid);

    add_dependency(&conn, &dependent.id, &blocker.id).unwrap();
    add_relation(&conn, &dependent.id, &blocker.id, "duplicate").unwrap();

    assert!(!all_dependencies_completed(&conn, &dependent.id).unwrap());

    let completed = crate::state::first_in_group(&conn, &pid, "completed").unwrap();
    update_state(&conn, &blocker.id, &completed.id).unwrap();
    assert!(all_dependencies_completed(&conn, &dependent.id).unwrap());

    // Removing the duplicate relation must not touch the blocking edge.
    remove_relation(&conn, &dependent.id, &blocker.id, "duplicate").unwrap();
    assert!(all_dependencies_completed(&conn, &dependent.id).unwrap());
    assert_eq!(
        list_dependencies(&conn, &dependent.id).unwrap(),
        vec![blocker.id.clone()]
    );
}

/// `list` computes filter signals over every candidate id. SQLite rejects a
/// statement with more bound variables than `SQLITE_LIMIT_VARIABLE_NUMBER`
/// (32766 by default). The id queries must chunk, and an edge or comment
/// count that lands in a later chunk must still come back.
#[test]
fn id_signal_queries_return_rows_from_later_batches() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let early = make_item(&conn, &pid, &sid);
    let late = make_item(&conn, &pid, &sid);
    let blocker = make_item(&conn, &pid, &sid);
    add_dependency(&conn, &early.id, &blocker.id).unwrap();
    add_dependency(&conn, &late.id, &blocker.id).unwrap();
    crate::comment::create(&conn, &late.id, "agent-1", "note").unwrap();

    // One batch is the live variable limit. Shrink it so a handful of ids
    // cannot fit in a single statement — same failure a 32k-item list hits
    // against the default cap.
    let batch: i32 = 3;
    conn.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_VARIABLE_NUMBER, batch)
        .unwrap();
    let mut ids = vec![early.id.clone()];
    while ids.len() < batch as usize {
        ids.push(format!("pad-{}", ids.len()));
    }
    ids.push(late.id.clone());
    ids.push(blocker.id.clone());
    assert!(
        ids.len() > batch as usize,
        "the id list must span more than one batch"
    );

    let edges = dependency_edges_for_items(&conn, &ids).unwrap();
    let mut dependents: Vec<_> = edges.iter().map(|(id, _, _)| id.clone()).collect();
    dependents.sort();
    let mut expected = vec![early.id.clone(), late.id.clone()];
    expected.sort();
    assert_eq!(dependents, expected);

    let fanin = dependency_fanin_for_items(&conn, &ids).unwrap();
    assert_eq!(fanin.get(&blocker.id), Some(&2));

    let counts = crate::comment::count_by_items(&conn, &ids).unwrap();
    assert_eq!(counts.get(&late.id), Some(&1));
    assert_eq!(counts.get(&early.id), None);
}
