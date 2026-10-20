use super::*;

// --- item #631: structural filters + shared annotation flags on list/search ---

#[test]
fn item_list_rows_always_carry_groom_style_annotation_flags() {
    let (_tmp, s) = harness();
    serde_json::from_str::<serde_json::Value>(
        &s.item(Parameters(empty_item_create("Solo"))).unwrap(),
    )
    .unwrap();

    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let row = &listed["items"].as_array().unwrap()[0];
    for key in [
        "stale",
        "unassigned",
        "overdue",
        "unestimated",
        "blocked_by",
        "depended_on_by_count",
        "possible_duplicates",
        "confirmed_duplicate",
        "has_comments",
        "stale_claim",
    ] {
        assert!(
            row.get(key).is_some(),
            "list row missing annotation flag `{key}`: {row}"
        );
    }
}

#[test]
fn item_list_filters_by_unassigned() {
    let (_tmp, s) = harness();
    let unassigned: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Unassigned"))).unwrap())
            .unwrap();
    let assigned: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Assigned".into()),
            assignee_agent: Some("agent-x".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            unassigned: Some(true),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let ids: Vec<&str> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&unassigned["id"].as_str().unwrap()));
    assert!(!ids.contains(&assigned["id"].as_str().unwrap()));
    assert_eq!(listed["total"], 1);
}

#[test]
fn item_list_filters_by_blocked() {
    let (_tmp, s) = harness();
    let dep: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Dep"))).unwrap()).unwrap();
    let blocked: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Blocked".into()),
            dependency_ids: Some(vec![dep["id"].as_str().unwrap().to_string()]),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            blocked: Some(true),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], blocked["id"]);
    assert_eq!(items[0]["blocked_by"].as_array().unwrap()[0], dep["id"]);
}

#[test]
fn item_list_filters_by_unestimated() {
    let (_tmp, s) = harness();
    let sized: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Sized".into()),
            metadata: Some(serde_json::json!({"size": "S"})),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let bare: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Bare"))).unwrap()).unwrap();

    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            unestimated: Some(true),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let ids: Vec<&str> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&bare["id"].as_str().unwrap()));
    assert!(!ids.contains(&sized["id"].as_str().unwrap()));
}

#[test]
fn item_list_filters_by_has_comments() {
    let (_tmp, s) = harness();
    let commented: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Commented"))).unwrap()).unwrap();
    let uncommented: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(empty_item_create("Uncommented")))
            .unwrap(),
    )
    .unwrap();
    s.comment(Parameters(CommentRequest {
        action: "create".into(),
        item_id: Some(commented["id"].as_str().unwrap().to_string()),
        body: Some("note".into()),
        ..Default::default()
    }))
    .unwrap();

    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            has_comments: Some(true),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let ids: Vec<&str> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&commented["id"].as_str().unwrap()));
    assert!(!ids.contains(&uncommented["id"].as_str().unwrap()));
}

/// The claim ledger stores no per-claim TTL — staleness is `now minus
/// heartbeat_at` exceeding `ttl_secs`, evaluated at read time against the
/// live default TTL (`AGENTFLARE_BACKEND_CLAIM_TTL_SECS`, 4h). Acquiring the
/// claim at `now=1` (epoch second 1) makes it unconditionally stale by the
/// time this test's real wall-clock `list` call evaluates it, regardless of
/// that default's magnitude.
#[test]
fn item_list_filters_by_stale_claim() {
    let (tmp, s) = harness();
    let stale: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("StaleClaim"))).unwrap())
            .unwrap();
    let fresh: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("NoClaim"))).unwrap()).unwrap();
    {
        let conn = backend_conn(&tmp);
        agentflare_backend::claim::acquire(
            &conn,
            stale["id"].as_str().unwrap(),
            "someone",
            1,
            14_400,
        )
        .unwrap();
    }

    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            stale_claim: Some(true),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let ids: Vec<&str> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&stale["id"].as_str().unwrap()));
    assert!(!ids.contains(&fresh["id"].as_str().unwrap()));
}

#[test]
fn item_list_combines_unassigned_and_blocked_filters() {
    let (_tmp, s) = harness();
    let dep: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Dep2"))).unwrap()).unwrap();
    // Blocked AND unassigned — should match both filters.
    let both: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("BlockedUnassigned".into()),
            dependency_ids: Some(vec![dep["id"].as_str().unwrap().to_string()]),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    // Blocked but assigned — must be excluded once `unassigned=true` is added.
    s.item(Parameters(ItemRequest {
        action: "create".into(),
        name: Some("BlockedAssigned".into()),
        assignee_agent: Some("agent-y".into()),
        dependency_ids: Some(vec![dep["id"].as_str().unwrap().to_string()]),
        ..Default::default()
    }))
    .unwrap();

    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            unassigned: Some(true),
            blocked: Some(true),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], both["id"]);
}

#[test]
fn item_search_filters_and_carries_annotation_flags() {
    let (_tmp, s) = harness();
    let sized: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Searchable Widget Sized".into()),
            metadata: Some(serde_json::json!({"size": "S"})),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let bare: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Searchable Widget Bare".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let searched: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "search".into(),
            query: Some("Widget".into()),
            unestimated: Some(true),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let items = searched["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], bare["id"]);
    assert!(items[0].get("unestimated").is_some());
    assert_ne!(items[0]["id"], sized["id"]);
    assert_eq!(searched["total"], 1);
}

#[test]
fn item_search_applies_structural_filter_before_the_requested_limit() {
    // Regression: a structural filter used to run in-memory (`retain`)
    // *after* the backend's BM25 query had already applied the caller's
    // `limit` at the SQL level. A higher-ranked non-matching row could
    // occupy that narrow SQL-level window and starve out a lower-ranked
    // row that actually matches the filter, even though the filtered
    // match exists in the corpus. Empirically "Bare" (shorter document)
    // outranks "Sized" for the query "Widget", so `limit: 1` combined
    // with `unestimated: false` (only "Sized" qualifies) reproduces it:
    // the pre-fix SQL-level LIMIT 1 fetches only "Bare", which the
    // filter then discards, leaving zero results even though "Sized"
    // matches and exists in the corpus.
    let (_tmp, s) = harness();
    let sized: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Searchable Widget Sized".into()),
            metadata: Some(serde_json::json!({"size": "S"})),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let bare: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Searchable Widget Bare".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let searched: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "search".into(),
            query: Some("Widget".into()),
            unestimated: Some(false),
            limit: Some(1),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let items = searched["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], sized["id"]);
    assert_ne!(items[0]["id"], bare["id"]);
    assert_eq!(searched["total"], 1);
}

#[test]
fn item_groom_flags_has_comments_and_stale_claim() {
    let (tmp, s) = harness();
    let item: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(empty_item_create("GroomAnnotated")))
            .unwrap(),
    )
    .unwrap();
    s.comment(Parameters(CommentRequest {
        action: "create".into(),
        item_id: Some(item["id"].as_str().unwrap().to_string()),
        body: Some("note".into()),
        ..Default::default()
    }))
    .unwrap();
    {
        let conn = backend_conn(&tmp);
        agentflare_backend::claim::acquire(
            &conn,
            item["id"].as_str().unwrap(),
            "someone",
            1,
            14_400,
        )
        .unwrap();
    }

    let groomed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "groom".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let entry = groomed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == item["id"])
        .expect("item present in groom shortlist");
    assert_eq!(entry["has_comments"], true);
    assert_eq!(entry["stale_claim"], true);
}

/// Structural filtering must still run over the whole candidate set (not
/// just the returned page) -- a fix-round regression guard for the
/// filter-signals/full-annotations split: filtering is cheap and must stay
/// pre-pagination, only the expensive display-only annotations move to
/// post-pagination.
#[test]
fn item_list_filters_before_pagination_across_a_multi_page_result() {
    let (_tmp, s) = harness();
    // Three assigned "noise" items sort first (see below), then one
    // unassigned target -- with limit=1 a naive "filter the first page"
    // implementation would page past the target before the unassigned
    // filter ever saw it.
    for n in 1..=3 {
        s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some(format!("Noise{n}")),
            assignee_agent: Some("agent-noise".into()),
            ..Default::default()
        }))
        .unwrap();
    }
    let target: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Target"))).unwrap()).unwrap();

    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            unassigned: Some(true),
            limit: Some(1),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], target["id"]);
    assert_eq!(listed["total"], 1);
}

/// `possible_duplicates` is a display-only annotation (it doesn't gate any
/// structural filter) -- computing it is O(n^2) over whatever set it's
/// given, so a fix round scoped it to just the returned page instead of the
/// whole filtered backlog (mirroring `groom`'s existing shortlist-scoped
/// duplicate detection). With `limit=2` and three near-identical names, the
/// third item never reaches page-annotation computation, so it must not
/// show up in the first two items' `possible_duplicates`.
#[test]
fn item_list_scopes_possible_duplicates_to_the_returned_page() {
    let (_tmp, s) = harness();
    let a: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(empty_item_create("Widget Alpha Duplicate")))
            .unwrap(),
    )
    .unwrap();
    let b: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(empty_item_create("Widget Beta Duplicate")))
            .unwrap(),
    )
    .unwrap();
    let c: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(empty_item_create("Widget Gamma Duplicate")))
            .unwrap(),
    )
    .unwrap();

    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            limit: Some(2),
            offset: Some(0),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let page_ids: Vec<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert!(page_ids.contains(&a["id"].as_str().unwrap()));
    assert!(page_ids.contains(&b["id"].as_str().unwrap()));
    assert!(!page_ids.contains(&c["id"].as_str().unwrap()));

    for item in items {
        let dups: Vec<&str> = item["possible_duplicates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            !dups.contains(&c["id"].as_str().unwrap()),
            "off-page item {} leaked into possible_duplicates: {dups:?}",
            c["id"]
        );
    }
}

/// Item #371: `list` dropped every item with an open `blocks` dependency.
/// Repro was a chain A ← B ← C (B depends on A, C depends on B) while B and C
/// were in progress under other agents. `search` still returned them with
/// `blocked_by`; `list` (including `blocked=true`) did not. The implicit
/// inbox filter must not hide those rows. `blocked=true` / `blocked=false`
/// partition the chain.
#[test]
fn item_list_returns_open_dependency_chain_with_blocked_by() {
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        agent: Some("claude-code".into()),
        ..Default::default()
    };

    let a: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Chain A"))).unwrap()).unwrap();
    let b: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Chain B"))).unwrap()).unwrap();
    let c: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Chain C"))).unwrap()).unwrap();
    let a_id = a["id"].as_str().unwrap().to_string();
    let b_id = b["id"].as_str().unwrap().to_string();
    let c_id = c["id"].as_str().unwrap().to_string();

    for (id, assignee) in [
        (&a_id, "claude-code"),
        (&b_id, "opencode"),
        (&c_id, "cursor"),
    ] {
        s.item(Parameters(ItemRequest {
            action: "update".into(),
            id: Some(id.clone()),
            assignee_agent: Some(assignee.into()),
            ..Default::default()
        }))
        .unwrap();
    }

    let started_state_id = {
        let conn = backend_conn(&tmp);
        let project_id = a["project_id"].as_str().unwrap();
        agentflare_backend::state::list_by_project(&conn, project_id)
            .unwrap()
            .into_iter()
            .find(|st| st.group_name == "started")
            .unwrap()
            .id
    };
    for id in [&a_id, &b_id, &c_id] {
        s.item(Parameters(ItemRequest {
            action: "update_state".into(),
            id: Some(id.clone()),
            state_id: Some(started_state_id.clone()),
            ..Default::default()
        }))
        .unwrap();
    }

    // add_relation id=X related_item_id=Y relation_type=blocks → X depends on Y.
    s.item(Parameters(ItemRequest {
        action: "add_relation".into(),
        id: Some(b_id.clone()),
        related_item_id: Some(a_id.clone()),
        relation_type: Some("blocks".into()),
        ..Default::default()
    }))
    .unwrap();
    s.item(Parameters(ItemRequest {
        action: "add_relation".into(),
        id: Some(c_id.clone()),
        related_item_id: Some(b_id.clone()),
        relation_type: Some("blocks".into()),
        ..Default::default()
    }))
    .unwrap();

    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            state_group: Some("started".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let by_id = |page: &serde_json::Value, id: &str| {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .unwrap_or_else(|| panic!("{id} missing from list: {page}"))
            .clone()
    };
    let row_b = by_id(&listed, &b_id);
    let row_c = by_id(&listed, &c_id);
    assert_eq!(
        row_b["blocked_by"].as_array().unwrap().as_slice(),
        [a["id"].clone()]
    );
    assert_eq!(
        row_c["blocked_by"].as_array().unwrap().as_slice(),
        [b["id"].clone()]
    );
    assert_eq!(by_id(&listed, &a_id)["depended_on_by_count"], 1);
    assert_eq!(row_b["depended_on_by_count"], 1);

    let blocked: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            state_group: Some("started".into()),
            blocked: Some(true),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let blocked_ids: Vec<&str> = blocked["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(blocked["total"], 2);
    assert!(blocked_ids.contains(&b_id.as_str()));
    assert!(blocked_ids.contains(&c_id.as_str()));
    assert!(!blocked_ids.contains(&a_id.as_str()));

    let unblocked: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            state_group: Some("started".into()),
            blocked: Some(false),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let unblocked_ids: Vec<&str> = unblocked["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert!(unblocked_ids.contains(&a_id.as_str()));
    assert!(!unblocked_ids.contains(&b_id.as_str()));
    assert!(!unblocked_ids.contains(&c_id.as_str()));
}

/// With enforcement off, `needs_input` is not provisioned: `update_state`
/// to it (and to any bogus group) errors cleanly, and `list` on it is empty.
#[test]
fn needs_input_group_without_enforce_errors_cleanly_and_lists_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        agent: Some("claude-code".into()),
        ..Default::default()
    };
    let a: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("NI"))).unwrap()).unwrap();
    for group in ["needs_input", "no_such_group"] {
        let err = s
            .item(Parameters(ItemRequest {
                action: "update_state".into(),
                id: Some(a["id"].as_str().unwrap().into()),
                state_group: Some(group.into()),
                ..Default::default()
            }))
            .unwrap_err();
        assert!(
            err.message.contains("no state in group"),
            "{group}: {err:?}"
        );
    }
    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            state_group: Some("needs_input".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(listed["total"], 0);
}

/// `[verify] enforce = true`: the project gets a needs_input state, an item
/// moves into it via `update_state`, `list` finds it there, `claim` refuses
/// it, and it moves back to started.
#[test]
fn needs_input_round_trip_with_enforce_on() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".agentflare")).unwrap();
    std::fs::write(
        repo.join(".agentflare").join("config.toml"),
        "[verify]\nenforce = true\n",
    )
    .unwrap();
    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(repo),
        agent: Some("claude-code".into()),
        ..Default::default()
    };
    let a: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("NI on"))).unwrap()).unwrap();
    let id = a["id"].as_str().unwrap().to_string();
    let set = |group: &str| {
        s.item(Parameters(ItemRequest {
            action: "update_state".into(),
            id: Some(id.clone()),
            state_group: Some(group.into()),
            ..Default::default()
        }))
    };
    let listed = |group: &str| -> serde_json::Value {
        serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "list".into(),
                state_group: Some(group.into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap()
    };
    set("needs_input").unwrap();
    assert_eq!(listed("needs_input")["total"], 1);
    // Open but non-dispatchable: search still finds it, the default groom
    // shortlist (backlog,unstarted) and the dispatch-facing reports do not
    // offer it, and standup/health handle it without error.
    let run = |action: &str, query: Option<&str>| {
        s.item(Parameters(ItemRequest {
            action: action.into(),
            query: query.map(Into::into),
            ..Default::default()
        }))
        .unwrap()
    };
    assert!(run("search", Some("NI on")).contains(&id));
    assert!(!run("groom", None).contains(&id));
    run("standup", None);
    run("health", None);
    let claim: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(claim["status"], "blocked", "{claim}");
    set("started").unwrap();
    assert_eq!(listed("needs_input")["total"], 0);
    assert_eq!(listed("started")["total"], 1);
}
