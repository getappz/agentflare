use clap::{Args, Subcommand};

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
            let log = crate::vent::paths::log_path();
            match crate::vent::capture::append(&log, None, severity, &tags, message.trim()) {
                Ok(id) => println!("vented {id}"),
                Err(e) => eprintln!("vent failed: {e}"),
            }
        }
        VentCmd::Consolidate => {
            let r = crate::vent::consolidate::consolidate();
            println!(
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
            );
            for id in r.items_created {
                println!("  filed item {id}");
            }
        }
        VentCmd::List { actionable } => {
            let conn = match agentflare_backend::db::open_db(&crate::vent::paths::backend_db_path())
            {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("cannot open backend: {e}");
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
                println!("no linked project — run an agentflare item/memory command here first");
                return;
            };
            match agentflare_backend::vent::list(&conn, &pid, actionable) {
                Ok(vents) => {
                    for v in vents {
                        println!(
                            "{}  seen×{}  {}  {}{}",
                            if v.actionable { "●" } else { "○" },
                            v.seen_count,
                            v.severity,
                            v.message.split('\n').next().unwrap_or(""),
                            v.item_id
                                .map(|i| format!("  → item {i}"))
                                .unwrap_or_default(),
                        );
                    }
                }
                Err(e) => eprintln!("list failed: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
