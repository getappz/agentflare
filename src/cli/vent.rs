use clap::{Args, Subcommand};

/// Capture and triage friction points hit during a session.
#[derive(Args)]
pub struct VentArgs {
    #[command(subcommand)]
    pub cmd: VentCmd,
}

#[derive(Subcommand)]
pub enum VentCmd {
    /// Log a friction vent (append-only; triaged at the next turn).
    Say {
        message: String,
        #[arg(long, default_value = "medium")]
        severity: String,
        #[arg(long = "tag")]
        tags: Vec<String>,
    },
    /// Triage buffered vents now (also run automatically once per turn).
    Consolidate,
    /// List (or file, with --title/--body) pending agentflare-core vents as
    /// ONE batched GitHub issue on getappz/agentflare.
    File {
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        body: Option<String>,
    },
    /// List triaged vents for this repo's project.
    List {
        #[arg(long)]
        actionable: bool,
    },
}

pub fn run(args: VentArgs) {
    match args.cmd {
        VentCmd::Say {
            message,
            severity,
            tags,
        } => {
            let severity = crate::vent::classify::normalize_severity(Some(&severity));
            match crate::vent::capture::append_routed(None, severity, &tags, message.trim()) {
                Ok((id, true)) => crate::ui::skip(&format!(
                    "vent {id} suppressed (same friction already vented recently)"
                )),
                Ok((id, false)) => crate::ui::success(&format!("vented {id}")),
                Err(e) => crate::ui::error(&format!("vent failed: {e}")),
            }
        }
        VentCmd::Consolidate => {
            let r = crate::vent::consolidate::consolidate();
            crate::ui::success(&format!(
                "consolidated {} vent(s) → {} item(s){}",
                r.consolidated,
                r.items_created.len(),
                if r.buffered_no_project > 0 {
                    format!(
                        " ({} buffered — no linked project yet)",
                        r.buffered_no_project
                    )
                } else {
                    String::new()
                }
            ));
            for id in r.items_created {
                crate::ui::step(&format!("filed item {id}"));
            }
            if r.escalated + r.acknowledged + r.resolved > 0 {
                crate::ui::info(&format!(
                    "escalations: {} re-escalated, {} acknowledged, {} resolved",
                    r.escalated, r.acknowledged, r.resolved
                ));
            }
        }
        VentCmd::File { title, body } => {
            let log = crate::vent::paths::global_log_path();
            let filed = crate::vent::paths::global_filed_path();
            let batch =
                crate::vent::file::pending_batch(&log, &filed, chrono::Utc::now().timestamp());
            if batch.is_empty() {
                crate::ui::skip("nothing to file");
                return;
            }
            let (title, body) = match (title, body) {
                (Some(title), Some(body)) => (title, body),
                (None, None) => {
                    println!("{} pending agentflare-core vent(s):", batch.len());
                    for g in &batch {
                        println!("  [{}] seen×{} {}", g.severity, g.seen_count, g.message);
                    }
                    crate::ui::info("re-run with --title and --body to file these as one GitHub issue");
                    return;
                }
                (title, body) => {
                    crate::ui::error(&format!(
                        "vent file: need both --title and --body to file (got title={}, body={})",
                        title.is_some(),
                        body.is_some()
                    ));
                    return;
                }
            };
            match crate::vent::file::create_github_issue(&title, &body) {
                Ok(url) => {
                    let keys: Vec<String> = batch.iter().map(|g| g.topic_key.clone()).collect();
                    let now = chrono::Utc::now().timestamp();
                    match crate::vent::file::mark_filed(&filed, &keys, &url, now) {
                        Ok(()) => crate::ui::success(&format!("filed {url} ({} vent(s))", keys.len())),
                        Err(e) => crate::ui::warning(&format!(
                            "filed {url} but failed to record state ({e}) — re-filing will \
                             create a duplicate issue until this is reconciled by hand"
                        )),
                    }
                }
                Err(e) => crate::ui::error(&format!("gh issue create failed: {e}")),
            }
        }
        VentCmd::List { actionable } => {
            let conn = match agentflare_backend::db::open_db(&crate::vent::paths::backend_db_path())
            {
                Ok(c) => c,
                Err(e) => {
                    crate::ui::error(&format!("cannot open backend: {e}"));
                    return;
                }
            };
            let link = crate::vent::paths::repo_root()
                .join(".agentflare")
                .join("project.json");
            let project_id = std::fs::read(&link)
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|v| {
                    v.get("project_id")
                        .and_then(|p| p.as_str().map(String::from))
                });
            let Some(pid) = project_id else {
                crate::ui::info("no linked project — run an agentflare item/memory command here first");
                return;
            };
            match agentflare_backend::vent::list(&conn, &pid, actionable) {
                Ok(vents) => {
                    for v in vents {
                        println!(
                            "{}  seen×{}  {}  {}{}{}",
                            if v.actionable { "●" } else { "○" },
                            v.seen_count,
                            v.severity,
                            v.message.split('\n').next().unwrap_or(""),
                            v.item_id
                                .map(|i| format!("  → item {i}"))
                                .unwrap_or_default(),
                            if v.escalation_state == "none" {
                                String::new()
                            } else {
                                format!("  [{} tier {}]", v.escalation_state, v.escalation_level)
                            },
                        );
                    }
                }
                Err(e) => crate::ui::error(&format!("list failed: {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn file_command_reports_nothing_when_batch_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let (log, filed) = (dir.path().join("v.jsonl"), dir.path().join("v.filed.json"));
        let batch = crate::vent::file::pending_batch(&log, &filed, chrono::Utc::now().timestamp());
        assert!(batch.is_empty(), "no log file yet means nothing pending");
    }

    #[test]
    fn say_via_append_routed_marks_agentflare_core_message_as_suppressed_on_repeat() {
        crate::paths::test_support::with_temp_home(|| {
            let (_, first) =
                crate::vent::capture::append_routed(None, "high", &[], "af-guard blocked a push")
                    .unwrap();
            let (_, second) =
                crate::vent::capture::append_routed(None, "high", &[], "af-guard blocked a push")
                    .unwrap();
            assert!(!first);
            assert!(second);
        });
    }

    #[test]
    fn append_then_read_back_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("v.jsonl");
        crate::vent::capture::append(&log, None, "high", &[], "roundtrip check").unwrap();
        let (lines, _) =
            crate::vent::consolidate::read_new_lines(&log, &dir.path().join("v.cursor"));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].message, "roundtrip check");
    }
}
