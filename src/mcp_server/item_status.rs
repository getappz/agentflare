//! `item(action="status")` -- split out of `item.rs` to keep it under the
//! repo's LOC gate. One-call status aggregator (item #298): item state +
//! its most recent dispatch job + PR CI status + recent daemon-log lines
//! mentioning it, so checking progress on a dispatched item doesn't need
//! `get` + `workflow(action="status")` + `check_merge` + a manual
//! `agentflare daemon logs | grep` round trip.

use super::*;

/// Default/max daemon-log-line count for `status` — mirrors the
/// `unwrap_or`+`clamp` pattern used elsewhere in `item.rs`; the daemon log
/// itself is unbounded (only truncated on daemon restart, see
/// `daemon::daemon_log_path`'s doc comment), so an unbounded return here
/// could dump an entire session's log.
const DEFAULT_STATUS_LOG_LINES: i64 = 20;
const MAX_STATUS_LOG_LINES: i64 = 200;

/// Builds `item_status`'s serializable PR summary from `PrCiStatus` (which
/// itself can't derive `Serialize`, see `PrStatusSummary`'s doc comment).
/// `pr_number_from_metadata` fills `number`/`url` whenever the CI-status
/// variant itself doesn't carry a PR number (`Merged`/`Pending`/`Unknown`),
/// since `push_and_open_pr` stores `metadata.pr.number` independently of
/// whatever CI state the PR is currently in.
fn pr_status_summary(
    item: &agentflare_backend::item::Item,
    repo_root: &std::path::Path,
) -> PrStatusSummary {
    let repo = crate::github::RepoId::resolve_from_remote(repo_root);
    let known_number = crate::worktree::pr_number_from_metadata(item);
    let url = |number: u64| {
        repo.as_ref()
            .map(|r| format!("https://github.com/{r}/pull/{number}"))
    };
    let (status, number, checks, labels) = match crate::worktree::pr_ci_status(item, repo_root) {
        crate::worktree::PrCiStatus::Merged => ("merged", known_number, vec![], vec![]),
        crate::worktree::PrCiStatus::Failing {
            number,
            checks,
            labels,
        } => ("failing", Some(number), checks, labels),
        crate::worktree::PrCiStatus::Pending { number, .. } => {
            ("pending", Some(number), vec![], vec![])
        }
        crate::worktree::PrCiStatus::Passing { number, labels, .. } => {
            ("passing", Some(number), vec![], labels)
        }
        crate::worktree::PrCiStatus::AwaitingReview { number, labels } => {
            ("awaiting_review", Some(number), vec![], labels)
        }
        crate::worktree::PrCiStatus::Behind { number, .. } => {
            ("behind", Some(number), vec![], vec![])
        }
        crate::worktree::PrCiStatus::Conflicting { number } => {
            ("conflicting", Some(number), vec![], vec![])
        }
        crate::worktree::PrCiStatus::Closed { number } => ("closed", Some(number), vec![], vec![]),
        crate::worktree::PrCiStatus::Unknown => ("unknown", known_number, vec![], vec![]),
    };
    PrStatusSummary {
        status: status.to_string(),
        url: number.and_then(url),
        number,
        checks,
        labels,
    }
}

/// Recent lines from the daemon's own log mentioning `item` -- either its
/// full id or `#<sequence_id>` (the form `supervisor.rs`'s own `eprintln!`
/// dispatch/skip messages use). Best-effort like the rest of `item_status`:
/// no daemon running yet (or a log path that doesn't exist) just yields no
/// lines rather than erroring, since a status check for a never-dispatched
/// item has nothing to find anyway.
fn recent_daemon_log_lines(item: &agentflare_backend::item::Item, limit: usize) -> Vec<String> {
    recent_log_lines_at(&crate::daemon::daemon_log_path(), item, limit)
}

/// `recent_daemon_log_lines`'s path-parameterized core -- split out so tests
/// can point it at a throwaway file instead of the real, shared
/// `daemon_log_path()` (which lives outside the test sandbox and may belong
/// to an actual running daemon on the machine).
fn recent_log_lines_at(
    path: &std::path::Path,
    item: &agentflare_backend::item::Item,
    limit: usize,
) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let seq_marker = format!("#{}", item.sequence_id);
    let matching: Vec<&str> = contents
        .lines()
        .filter(|line| {
            line_has_sequence_marker(line, &seq_marker) || line.contains(item.id.as_str())
        })
        .collect();
    let start = matching.len().saturating_sub(limit);
    matching[start..].iter().map(|s| s.to_string()).collect()
}

/// True if `line` contains `seq_marker` (e.g. `"#298"`) as a standalone
/// token rather than as a prefix of a longer number -- so item #9's status
/// doesn't pull in log lines about #90, #95, or #900. The marker's leading
/// `#` already gives a left boundary; only the right side (the character
/// immediately after the digits, if any) needs checking.
fn line_has_sequence_marker(line: &str, seq_marker: &str) -> bool {
    line.match_indices(seq_marker).any(|(idx, _)| {
        !line[idx + seq_marker.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
    })
}

impl AgentflareMcp {
    pub(crate) fn item_status(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let raw = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for status", None))?;
        if raw.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        let (item, state) = self.with_backend_db(|conn| {
            let id = self.resolve_item_id(conn, &raw)?;
            let item = agentflare_backend::item::get(conn, &id).map_err(map_backend_err)?;
            let state =
                agentflare_backend::state::get(conn, &item.state_id).map_err(map_backend_err)?;
            Ok::<_, ErrorData>((item, state))
        })??;

        let job = self.latest_job_for_item(&item.id);
        let pr = pr_status_summary(&item, &self.worktree_repo_root());
        let log_limit = req
            .limit
            .unwrap_or(DEFAULT_STATUS_LOG_LINES)
            .clamp(1, MAX_STATUS_LOG_LINES) as usize;
        let log_lines = recent_daemon_log_lines(&item, log_limit);

        let resp = ItemStatusResponse {
            id: item.id,
            sequence_id: item.sequence_id,
            name: item.name,
            state: state.name,
            state_group: state.group_name,
            priority: item.priority,
            assignee_agent: item.assignee_agent,
            updated_at: item.updated_at,
            job,
            pr,
            log_lines,
        };
        Ok(serde_json::to_string_pretty(&resp).unwrap_or_default())
    }
}

#[cfg(test)]
mod status_log_line_tests {
    use super::*;

    fn item_fixture(id: &str, sequence_id: i64) -> agentflare_backend::item::Item {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "project_id": "proj",
            "state_id": "state",
            "name": "Test",
            "description": "",
            "priority": "none",
            "parent_id": null,
            "assignee_agent": null,
            "sequence_id": sequence_id,
            "sort_order": 0.0,
            "started_at": null,
            "completed_at": null,
            "archived_at": null,
            "external_source": null,
            "external_id": null,
            "metadata": "{}",
            "created_at": 0,
            "updated_at": 0,
            "deleted_at": null,
            "start_date": null,
            "due_date": null,
        }))
        .unwrap()
    }

    // `recent_log_lines_at` takes a path parameter specifically so this test
    // never touches the real, shared `daemon::daemon_log_path()` -- that path
    // lives outside the test sandbox and may belong to an actual running
    // daemon on the machine.
    #[test]
    fn recent_log_lines_at_filters_by_sequence_id_and_item_id_and_respects_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemon.log");
        std::fs::write(
            &path,
            "unrelated line\n\
             agentflare-supervisor: item #298 (abc-123) dispatched\n\
             another unrelated line\n\
             agentflare-supervisor: item #298 (abc-123) done\n\
             mentions abc-123 directly\n\
             item #77 (other) noise\n",
        )
        .unwrap();
        let item = item_fixture("abc-123", 298);

        let all = recent_log_lines_at(&path, &item, 10);
        assert_eq!(all.len(), 3, "{all:?}");
        assert!(all[0].contains("dispatched"));
        assert!(all[2].contains("mentions abc-123"));

        // limit=2 keeps the *most recent* matches, not the first two.
        let capped = recent_log_lines_at(&path, &item, 2);
        assert_eq!(capped.len(), 2);
        assert!(capped[0].contains("done"), "{capped:?}");
        assert!(capped[1].contains("mentions abc-123"));
    }

    #[test]
    fn recent_log_lines_at_does_not_match_sequence_id_as_a_number_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("daemon.log");
        std::fs::write(
            &path,
            "agentflare-supervisor: item #9 (xyz-9) dispatched\n\
             agentflare-supervisor: item #90 (other-90) dispatched\n\
             agentflare-supervisor: item #95 (other-95) dispatched\n\
             agentflare-supervisor: item #900 (other-900) dispatched\n\
             agentflare-supervisor: item #9 (xyz-9) done\n",
        )
        .unwrap();
        let item = item_fixture("xyz-9", 9);

        let matches = recent_log_lines_at(&path, &item, 10);
        assert_eq!(matches.len(), 2, "{matches:?}");
        assert!(matches.iter().all(|l| l.contains("#9 ")), "{matches:?}");
    }

    #[test]
    fn recent_log_lines_at_returns_empty_when_the_file_does_not_exist() {
        let item = item_fixture("abc-123", 298);
        assert!(
            recent_log_lines_at(
                std::path::Path::new("/nonexistent/path/daemon.log"),
                &item,
                10
            )
            .is_empty()
        );
    }
}
