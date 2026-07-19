use crate::vent::capture::VentLine;
use crate::vent::classify::{classify, severity_rank, topic_key};
use crate::vent::paths;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Default)]
pub struct ConsolidateReport {
    pub consolidated: usize,
    pub items_created: Vec<String>,
    pub noise: usize,
    pub buffered_no_project: usize,
}

pub fn read_new_lines(log_path: &Path, cursor_path: &Path) -> (Vec<VentLine>, u64) {
    let mut offset = std::fs::read_to_string(cursor_path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let Ok(mut file) = std::fs::File::open(log_path) else {
        return (Vec::new(), offset);
    };
    let meta_len = file.metadata().map_or(0, |m| m.len());
    if meta_len < offset {
        offset = 0;
    }
    if meta_len == offset {
        return (Vec::new(), offset);
    }
    let _ = file.seek(SeekFrom::Start(offset));
    let reader = BufReader::new(&file);
    let mut lines = Vec::new();
    let mut bytes_read: u64 = 0;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        bytes_read += line.len() as u64 + 1;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<VentLine>(&line) {
            lines.push(v);
        }
    }
    (lines, offset + bytes_read)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn truncate(s: &str, max: usize) -> String {
    let one_line = s.split('\n').next().unwrap_or(s).trim();
    if one_line.chars().count() <= max {
        one_line.to_string()
    } else {
        format!("{}…", one_line.chars().take(max).collect::<String>())
    }
}

pub fn consolidate_core(
    conn: &rusqlite::Connection,
    project_id: &str,
    default_state_id: &str,
    log_path: &Path,
    cursor_path: &Path,
) -> std::io::Result<ConsolidateReport> {
    let (lines, new_offset) = read_new_lines(log_path, cursor_path);
    let mut report = ConsolidateReport::default();
    if lines.is_empty() {
        return Ok(report);
    }

    struct Group {
        message: String,
        severity: String,
        count: i64,
        first_event: String,
    }
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for l in &lines {
        let key = topic_key(&l.message);
        let g = groups.entry(key).or_insert_with(|| Group {
            message: l.message.clone(),
            severity: l.severity.clone(),
            count: 0,
            first_event: l.event_id.clone(),
        });
        g.count += 1;
        g.message = l.message.clone();
        if severity_rank(&l.severity) > severity_rank(&g.severity) {
            g.severity = l.severity.clone();
        }
    }

    for (key, g) in groups {
        report.consolidated += g.count as usize;
        let tags_json = "[]";
        let out = match agentflare_backend::vent::upsert(
            conn,
            project_id,
            &g.message,
            &g.severity,
            tags_json,
            &key,
            &g.first_event,
            g.count,
            now(),
        ) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("[vent] upsert failed: {e}");
                continue;
            }
        };
        let actionable = classify(&g.severity, out.seen_count, &g.message);
        let _ = agentflare_backend::vent::set_actionable(conn, &out.id, actionable);
        if !actionable {
            report.noise += g.count as usize;
            continue;
        }
        if out.existing_item_id.is_some() {
            continue;
        }
        let metadata = serde_json::json!({
            "source": "vent",
            "topic_key": key,
            "severity": g.severity,
            "seen_count": out.seen_count,
            "first_event_id": g.first_event,
        })
        .to_string();
        let input = agentflare_backend::item::CreateItem {
            project_id: project_id.to_string(),
            state_id: default_state_id.to_string(),
            name: format!("[vent] {}", truncate(&g.message, 72)),
            description: Some(format!(
                "{}\n\n---\nsource: vent · severity: {} · seen: {}",
                g.message, g.severity, out.seen_count
            )),
            priority: None,
            parent_id: None,
            assignee_agent: None,
            sort_order: None,
            external_source: None,
            external_id: None,
            metadata: Some(metadata),
            label_ids: vec![],
            assignee_ids: vec![],
            dependency_ids: vec![],
        };
        match agentflare_backend::item::create(conn, input) {
            Ok(item) => {
                let _ = agentflare_backend::vent::link_item(conn, &out.id, &item.id);
                report.items_created.push(item.id);
            }
            Err(e) => eprintln!("[vent] item::create failed: {e}"),
        }
    }

    let _ = std::fs::write(cursor_path, new_offset.to_string());
    Ok(report)
}

pub fn consolidate() -> ConsolidateReport {
    let empty = ConsolidateReport::default();
    let conn = match agentflare_backend::db::open_db(&paths::backend_db_path()) {
        Ok(c) => c,
        Err(_) => return empty,
    };
    let link_file = paths::repo_root().join(".agentflare").join("project.json");
    let Ok(bytes) = std::fs::read(&link_file) else {
        let (lines, _) = read_new_lines(&paths::log_path(), &paths::cursor_path());
        return ConsolidateReport {
            buffered_no_project: lines.len(),
            ..empty
        };
    };
    let Ok(link) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return empty;
    };
    let Some(project_id) = link.get("project_id").and_then(|v| v.as_str()) else {
        return empty;
    };
    let Ok(project) = agentflare_backend::project::get(&conn, project_id) else {
        return empty;
    };
    let default_state = agentflare_backend::state::list_by_project(&conn, &project.id)
        .ok()
        .and_then(|states| states.into_iter().find(|s| s.is_default));
    let Some(state) = default_state else {
        return empty;
    };
    consolidate_core(
        &conn,
        &project.id,
        &state.id,
        &paths::log_path(),
        &paths::cursor_path(),
    )
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentflare_backend::db::open_in_memory;

    fn seed(conn: &rusqlite::Connection) -> (String, String) {
        conn.execute(
            "INSERT INTO workspaces (id,name,slug,item_label,created_at,updated_at) VALUES ('w','W','w','Item',1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO projects (id,workspace_id,name,identifier,created_at,updated_at) VALUES ('p','w','P','P',1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO states (id,project_id,name,group_name,sequence,is_default,created_at,updated_at) VALUES ('s','p','Backlog','backlog',1.0,1,1,1)",
            [],
        )
        .unwrap();
        ("p".into(), "s".into())
    }

    fn write_lines(log: &std::path::Path, msgs: &[(&str, &str)]) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .unwrap();
        for (sev, m) in msgs {
            let line = crate::vent::capture::VentLine {
                event_id: crate::vent::event_id(m),
                ts: "t".into(),
                session: None,
                severity: (*sev).to_string(),
                tags: vec![],
                message: (*m).to_string(),
            };
            writeln!(f, "{}", serde_json::to_string(&line).unwrap()).unwrap();
        }
    }

    #[test]
    fn actionable_creates_one_item_noise_creates_none() {
        let conn = open_in_memory().unwrap();
        let (p, s) = seed(&conn);
        let dir = tempfile::tempdir().unwrap();
        let (log, cur) = (dir.path().join("v.jsonl"), dir.path().join("v.cursor"));
        write_lines(
            &log,
            &[
                ("high", "server crash on boot"),
                ("low", "just a calm note"),
            ],
        );
        let rep = consolidate_core(&conn, &p, &s, &log, &cur).unwrap();
        assert_eq!(rep.items_created.len(), 1, "only the actionable one");
        assert_eq!(rep.noise, 1);
        let items: i64 = conn
            .query_row("SELECT count(*) FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(items, 1);
    }

    #[test]
    fn consolidate_is_idempotent_via_cursor() {
        let conn = open_in_memory().unwrap();
        let (p, s) = seed(&conn);
        let dir = tempfile::tempdir().unwrap();
        let (log, cur) = (dir.path().join("v.jsonl"), dir.path().join("v.cursor"));
        write_lines(&log, &[("high", "boom happened")]);
        let r1 = consolidate_core(&conn, &p, &s, &log, &cur).unwrap();
        let r2 = consolidate_core(&conn, &p, &s, &log, &cur).unwrap();
        assert_eq!(r1.items_created.len(), 1);
        assert_eq!(r2.consolidated, 0, "cursor consumed the line");
        let items: i64 = conn
            .query_row("SELECT count(*) FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(items, 1, "no duplicate item on rerun");
    }

    #[test]
    fn within_turn_spiral_collapses_to_one_item() {
        let conn = open_in_memory().unwrap();
        let (p, s) = seed(&conn);
        let dir = tempfile::tempdir().unwrap();
        let (log, cur) = (dir.path().join("v.jsonl"), dir.path().join("v.cursor"));
        let spiral: Vec<(&str, &str)> =
            std::iter::repeat_n(("high", "the same broken thing"), 43).collect();
        write_lines(&log, &spiral);
        let rep = consolidate_core(&conn, &p, &s, &log, &cur).unwrap();
        assert_eq!(rep.items_created.len(), 1, "43 identical vents → one item");
        let seen: i64 = conn
            .query_row("SELECT seen_count FROM vents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(seen, 43);
    }
}
