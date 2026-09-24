use super::*;

const TTL: i64 = 14_400;
/// A pid that can't be running (above any real pid_max).
const DEAD_PID: u32 = u32::MAX - 7;

fn backend() -> Connection {
    agentflare_backend::db::open_in_memory().unwrap()
}

fn sessions_db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    crate::sessions::migrate(&c).unwrap();
    c
}

fn test_queue() -> agentflare_jobs::Queue {
    let dir = tempfile::tempdir().unwrap().keep();
    agentflare_jobs::Queue::open_memory(dir.join("logs")).unwrap()
}

fn seed_project(conn: &Connection) -> String {
    let ws = agentflare_backend::workspace::create(
        conn,
        agentflare_backend::workspace::CreateWorkspace {
            name: "Liveness".into(),
            slug: "liveness".into(),
            owner_agent: None,
            item_label: None,
        },
    )
    .unwrap();
    let project = agentflare_backend::project::create(
        conn,
        agentflare_backend::project::CreateProject {
            workspace_id: ws.id.clone(),
            name: "Liveness".into(),
            identifier: "LIV".into(),
            external_source: None,
            external_id: None,
        },
    )
    .unwrap();
    for name in [
        crate::supervisor::READY_LABEL,
        crate::supervisor::DISPATCHED_LABEL,
        crate::supervisor::NEEDS_MANUAL_LABEL,
        crate::supervisor::PAUSED_LABEL,
    ] {
        agentflare_backend::label::create(
            conn,
            agentflare_backend::label::CreateLabel {
                project_id: Some(project.id.clone()),
                workspace_id: ws.id.clone(),
                name: name.into(),
                color: None,
                parent_id: None,
                sort_order: None,
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
    }
    project.id
}

/// Creates an item and claims it for `owner` the way a dispatch does
/// (`item::claim`: started state, assignee = owner).
fn claimed_item(conn: &Connection, project_id: &str, name: &str, owner: &str, now: i64) -> String {
    let state_id = agentflare_backend::state::list_by_project(conn, project_id)
        .unwrap()
        .into_iter()
        .find(|s| s.is_default)
        .unwrap()
        .id;
    let id = agentflare_backend::item::create(
        conn,
        agentflare_backend::item::CreateItem {
            project_id: project_id.into(),
            state_id,
            name: name.into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
            sort_order: None,
            external_source: None,
            external_id: None,
            metadata: None,
            label_ids: vec![],
            assignee_ids: vec![],
            dependency_ids: vec![],
            start_date: None,
            due_date: None,
        },
    )
    .unwrap()
    .id;
    assert!(matches!(
        agentflare_backend::item::claim(conn, &id, owner, now, TTL).unwrap(),
        agentflare_backend::item::ClaimOutcome::Acquired
    ));
    id
}

fn touch(sessions: &Connection, key: &str, pid: u32, now: i64) {
    crate::sessions::touch(
        sessions,
        &crate::sessions::Touch {
            key,
            pid: Some(pid),
            ..Default::default()
        },
        now,
    )
    .unwrap();
}

fn owner_of(conn: &Connection, item_id: &str) -> Option<String> {
    agentflare_backend::claim::current_owner(conn, item_id)
}

fn has_label(conn: &Connection, item_id: &str, project_id: &str, name: &str) -> bool {
    let label = agentflare_backend::label::list_by_project(conn, project_id)
        .unwrap()
        .into_iter()
        .find(|l| l.name == name)
        .unwrap();
    agentflare_backend::item::list_labels(conn, item_id)
        .unwrap()
        .contains(&label.id)
}

#[test]
fn a_dead_sessions_claim_is_released_on_the_second_tick_and_a_live_one_is_kept() {
    let conn = backend();
    let sessions = sessions_db();
    let project = seed_project(&conn);
    let now = 1_000_000;
    let dead = claimed_item(&conn, &project, "dead", "claude-code:dead-session", now);
    let live = claimed_item(&conn, &project, "live", "claude-code:live-session", now);
    touch(&sessions, "claude-code:dead-session", DEAD_PID, now);
    touch(
        &sessions,
        "claude-code:live-session",
        std::process::id(),
        now,
    );
    let mut memory = SweepMemory::default();

    assert!(sweep(&conn, Some(&sessions), None, &mut memory, now + 12).is_empty());
    assert!(
        owner_of(&conn, &dead).is_some(),
        "one dead sighting is not enough"
    );

    let released = sweep(&conn, Some(&sessions), None, &mut memory, now + 24);
    assert_eq!(released.len(), 1, "{released:?}");
    assert_eq!(released[0].item_id, dead);
    assert!(released[0].redispatched);
    assert_eq!(owner_of(&conn, &dead), None);
    assert_eq!(
        owner_of(&conn, &live).as_deref(),
        Some("claude-code:live-session")
    );
    // Re-armed for the discovery tick, with the one-line explanation.
    assert!(has_label(
        &conn,
        &dead,
        &project,
        crate::supervisor::READY_LABEL
    ));
    let comments = agentflare_backend::comment::list_by_item(&conn, &dead).unwrap();
    assert!(
        comments.iter().any(|c| c
            .body
            .contains("claim released: owner claude-code:dead-session is no longer running")),
        "{comments:?}"
    );
}

#[test]
fn a_dead_verdict_must_repeat_on_consecutive_ticks() {
    let conn = backend();
    let sessions = sessions_db();
    let project = seed_project(&conn);
    let now = 1_000_000;
    let item = claimed_item(&conn, &project, "flappy", "codex:s", now);
    let mut memory = SweepMemory::default();

    touch(&sessions, "codex:s", DEAD_PID, now);
    assert!(sweep(&conn, Some(&sessions), None, &mut memory, now + 12).is_empty());
    // The session re-registered under a live process in between.
    touch(&sessions, "codex:s", std::process::id(), now + 20);
    assert!(sweep(&conn, Some(&sessions), None, &mut memory, now + 24).is_empty());
    touch(&sessions, "codex:s", DEAD_PID, now + 30);
    assert!(sweep(&conn, Some(&sessions), None, &mut memory, now + 36).is_empty());
    assert!(owner_of(&conn, &item).is_some());
    assert_eq!(
        sweep(&conn, Some(&sessions), None, &mut memory, now + 48).len(),
        1
    );
}

#[test]
fn a_job_owners_claim_is_released_once_its_job_row_is_terminal() {
    let conn = backend();
    let sessions = sessions_db();
    let queue = test_queue();
    let project = seed_project(&conn);
    let now = 1_000_000;
    let job = queue
        .enqueue(
            &agentflare_jobs::AgentJob::new("work")
                .in_process()
                .max_retries(0)
                .args(["placeholder", "claude-code"]),
        )
        .unwrap();
    queue.dequeue().unwrap().expect("job is running");
    let owner = format!("claude-code:{}", job.id);
    let item = claimed_item(&conn, &project, "job item", &owner, now);
    let mut memory = SweepMemory::default();

    // Running, no session row yet: live.
    for tick in 1..=3 {
        assert!(
            sweep(
                &conn,
                Some(&sessions),
                Some(&queue),
                &mut memory,
                now + tick * 12
            )
            .is_empty()
        );
    }
    queue.fail(&job.id, "boom", None, true).unwrap();
    assert!(sweep(&conn, Some(&sessions), Some(&queue), &mut memory, now + 48).is_empty());
    let released = sweep(&conn, Some(&sessions), Some(&queue), &mut memory, now + 60);
    assert_eq!(released.len(), 1);
    assert!(released[0].reason.contains("failed"), "{:?}", released[0]);
    assert_eq!(owner_of(&conn, &item), None);
}

#[test]
fn a_running_job_whose_process_exited_is_dead_but_this_processes_own_job_never_is() {
    let sessions = sessions_db();
    let queue = test_queue();
    let job = queue
        .enqueue(&agentflare_jobs::AgentJob::new("work").in_process())
        .unwrap();
    queue.dequeue().unwrap();
    let owner = format!("claude-code:{}", job.id);
    assert_eq!(
        judge_owner(&owner, 0, 10, Some(&sessions), Some(&queue)),
        OwnerLiveness::Live
    );
    touch(&sessions, &owner, DEAD_PID, 5);
    assert!(matches!(
        judge_owner(&owner, 0, 10, Some(&sessions), Some(&queue)),
        OwnerLiveness::Dead(_)
    ));
    queue.fail(&job.id, "done", None, true).unwrap();
    let _running_here = agentflare_jobs::cancel::register(&job.id, || false);
    assert_eq!(
        judge_owner(&owner, 0, 10, Some(&sessions), Some(&queue)),
        OwnerLiveness::Live,
        "a job executing in this process is never judged dead"
    );
}

#[test]
fn fallback_pid_owners_need_a_dead_pid_and_a_silent_claim() {
    let owner = format!("claude-code:{DEAD_PID}-0123456789abcdef");
    let now = 100_000;
    assert_eq!(
        judge_owner(&owner, now - 60, now, None, None),
        OwnerLiveness::Unknown,
        "recently heartbeated: may be a pid on another host"
    );
    assert!(matches!(
        judge_owner(
            &owner,
            now - crate::sessions::STALE_AFTER_SECS - 1,
            now,
            None,
            None
        ),
        OwnerLiveness::Dead(_)
    ));
    let alive = format!("claude-code:{}-0123456789abcdef", std::process::id());
    assert_eq!(judge_owner(&alive, 0, now, None, None), OwnerLiveness::Live);
    // Any other owner shape is unknown: TTL behavior as before.
    assert_eq!(
        judge_owner("cli:my-session", 0, now, None, None),
        OwnerLiveness::Unknown
    );
}

#[test]
fn a_paused_items_released_claim_is_not_requeued() {
    let conn = backend();
    let sessions = sessions_db();
    let project = seed_project(&conn);
    let now = 1_000_000;
    let item = claimed_item(&conn, &project, "parked", "codex:gone", now);
    let paused = agentflare_backend::label::list_by_project(&conn, &project)
        .unwrap()
        .into_iter()
        .find(|l| l.name == crate::supervisor::PAUSED_LABEL)
        .unwrap();
    agentflare_backend::item::add_label(&conn, &item, &paused.id).unwrap();
    touch(&sessions, "codex:gone", DEAD_PID, now);
    let mut memory = SweepMemory::default();
    sweep(&conn, Some(&sessions), None, &mut memory, now + 12);
    let released = sweep(&conn, Some(&sessions), None, &mut memory, now + 24);
    assert_eq!(released.len(), 1);
    assert!(!released[0].redispatched);
    assert!(!has_label(
        &conn,
        &item,
        &project,
        crate::supervisor::READY_LABEL
    ));
}
