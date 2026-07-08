use crate::paths::home;
use rusqlite::{params, Connection, Result as SqlResult};
use std::path::PathBuf;

const SCHEMA_VERSION: i32 = 1;

pub struct CooldownRow {
    pub agent: String,
    pub profile: String,
    pub until: String,
    pub reason: Option<String>,
}

pub struct ProfileHealth {
    pub agent: String,
    pub profile: String,
    pub status: String,
    pub error_count_1h: i32,
    pub penalty: f64,
    pub last_used_at: Option<String>,
}

fn db_path() -> PathBuf {
    home().join(".local").join("share").join("agentflare").join("auth.db")
}

pub fn migrate(conn: &Connection) -> SqlResult<()> {
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap_or(0);
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS profile_health (
            agent       TEXT NOT NULL,
            profile     TEXT NOT NULL,
            status      TEXT NOT NULL DEFAULT 'healthy',
            error_count_1h INTEGER NOT NULL DEFAULT 0,
            penalty     REAL NOT NULL DEFAULT 0.0,
            last_error_time TEXT,
            last_used_at TEXT,
            updated_at  TEXT NOT NULL,
            PRIMARY KEY (agent, profile)
        );
        CREATE TABLE IF NOT EXISTS cooldowns (
            agent   TEXT NOT NULL,
            profile TEXT NOT NULL,
            until   TEXT NOT NULL,
            reason  TEXT,
            PRIMARY KEY (agent, profile)
        );
        CREATE TABLE IF NOT EXISTS aliases (
            agent   TEXT NOT NULL,
            alias   TEXT NOT NULL,
            profile TEXT NOT NULL,
            PRIMARY KEY (agent, alias)
        );
        CREATE TABLE IF NOT EXISTS projects (
            path    TEXT NOT NULL,
            agent   TEXT NOT NULL,
            profile TEXT NOT NULL,
            PRIMARY KEY (path, agent)
        );
        CREATE TABLE IF NOT EXISTS rotation_state (
            agent TEXT PRIMARY KEY,
            last_profile TEXT,
            algorithm TEXT NOT NULL DEFAULT 'smart'
        );",
    )?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

pub fn open_or_rebuild() -> Connection {
    let path = db_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let conn = Connection::open(&path).expect("open auth.db");
    migrate(&conn).expect("migrate auth.db");
    conn
}

fn now_iso() -> String {
    // Use space separator to match SQLite datetime('now') format
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn is_older_than_1h(timestamp: &str, now: &str) -> bool {
    use chrono::NaiveDateTime;
    if let (Ok(ts), Ok(n)) = (
        NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%d %H:%M:%S"),
        NaiveDateTime::parse_from_str(now, "%Y-%m-%d %H:%M:%S"),
    ) {
        (n - ts).num_hours() >= 1
    } else {
        true
    }
}

fn penalty_for_error(msg: &str) -> f64 {
    let m = msg.to_lowercase();
    if m.contains("429") || m.contains("rate limit") || m.contains("too many requests") {
        10.0
    } else if m.contains("401") || m.contains("403") || m.contains("unauthorized") {
        100.0
    } else if m.contains("timeout") || m.contains("deadline exceeded") {
        5.0
    } else if m.contains("500") || m.contains("502") || m.contains("503") || m.contains("504") {
        5.0
    } else {
        3.0
    }
}

fn decay_penalty(conn: &Connection, agent: &str, profile: &str) -> f64 {
    let row = conn.query_row(
        "SELECT penalty, last_error_time FROM profile_health WHERE agent = ?1 AND profile = ?2",
        params![agent, profile],
        |row| Ok((row.get::<_, f64>(0)?, row.get::<_, Option<String>>(1)?)),
    );
    match row {
        Ok((penalty, Some(last_time))) => {
            use chrono::NaiveDateTime;
            let now = now_iso();
            if let (Ok(ts), Ok(n)) = (
                NaiveDateTime::parse_from_str(&last_time, "%Y-%m-%d %H:%M:%S"),
                NaiveDateTime::parse_from_str(&now, "%Y-%m-%d %H:%M:%S"),
            ) {
                let minutes = (n - ts).num_minutes().max(0) as f64;
                let decay_intervals = (minutes / 5.0).floor();
                penalty * 0.8_f64.powf(decay_intervals)
            } else {
                penalty
            }
        }
        _ => 0.0,
    }
}

pub fn record_error(conn: &Connection, agent: &str, profile: &str, error_msg: &str) {
    let penalty = penalty_for_error(error_msg);
    let now = now_iso();
    let existing = conn.query_row(
        "SELECT error_count_1h, last_error_time FROM profile_health WHERE agent = ?1 AND profile = ?2",
        params![agent, profile],
        |row| {
            Ok((
                row.get::<_, i32>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        },
    );
    let new_count = match existing {
        Ok((count, Some(last_time))) => {
            if is_older_than_1h(&last_time, &now) {
                1
            } else {
                count + 1
            }
        }
        _ => 1,
    };
    let decayed = decay_penalty(conn, agent, profile);
    let new_penalty = decayed + penalty;
    let status = if new_count >= 5 {
        "critical"
    } else if new_count >= 1 {
        "warning"
    } else {
        "healthy"
    };
    conn.execute(
        "INSERT INTO profile_health (agent, profile, status, error_count_1h, penalty, last_error_time, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(agent, profile) DO UPDATE SET
         status = excluded.status, error_count_1h = excluded.error_count_1h,
         penalty = excluded.penalty, last_error_time = excluded.last_error_time,
         updated_at = excluded.updated_at",
        params![agent, profile, status, new_count, new_penalty, now, now],
    )
    .ok();
}

/// Stamp a profile as just-activated so `smart_pick`'s recency term is real.
pub fn touch_last_used(conn: &Connection, agent: &str, profile: &str) {
    let now = now_iso();
    conn.execute(
        "INSERT INTO profile_health (agent, profile, status, error_count_1h, penalty, last_used_at, updated_at)
         VALUES (?1, ?2, 'healthy', 0, 0.0, ?3, ?4)
         ON CONFLICT(agent, profile) DO UPDATE SET
         last_used_at = excluded.last_used_at, updated_at = excluded.updated_at",
        params![agent, profile, now, now],
    )
    .ok();
}

pub fn set_cooldown(conn: &Connection, agent: &str, profile: &str, minutes: u32, reason: &str) {
    let until = chrono::Utc::now() + chrono::Duration::minutes(minutes as i64);
    conn.execute(
        "INSERT INTO cooldowns (agent, profile, until, reason) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(agent, profile) DO UPDATE SET until = excluded.until, reason = excluded.reason",
        params![
            agent,
            profile,
            until.format("%Y-%m-%d %H:%M:%S").to_string(),
            reason
        ],
    )
    .ok();
}

pub fn list_cooldowns(conn: &Connection, agent: Option<&str>) -> Vec<CooldownRow> {
    let (sql, has_agent) = if let Some(_a) = agent {
        (
            "SELECT agent, profile, until, reason FROM cooldowns WHERE agent = ?1 AND until > datetime('now')",
            true,
        )
    } else {
        (
            "SELECT agent, profile, until, reason FROM cooldowns WHERE until > datetime('now')",
            false,
        )
    };
    let mut stmt = conn.prepare(sql).unwrap();
    if has_agent {
        stmt.query_map(params![agent.unwrap()], |row| {
            Ok(CooldownRow {
                agent: row.get(0)?,
                profile: row.get(1)?,
                until: row.get(2)?,
                reason: row.get(3)?,
            })
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    } else {
        stmt.query_map([], |row| {
            Ok(CooldownRow {
                agent: row.get(0)?,
                profile: row.get(1)?,
                until: row.get(2)?,
                reason: row.get(3)?,
            })
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }
}

pub fn clear_cooldown(conn: &Connection, agent: &str, profile: &str) {
    conn.execute(
        "DELETE FROM cooldowns WHERE agent = ?1 AND profile = ?2",
        params![agent, profile],
    )
    .ok();
}

pub fn get_health(conn: &Connection, agent: &str, profile: &str) -> ProfileHealth {
    conn.query_row(
        "SELECT agent, profile, status, error_count_1h, penalty, last_used_at FROM profile_health WHERE agent = ?1 AND profile = ?2",
        params![agent, profile],
        |row| {
            Ok(ProfileHealth {
                agent: row.get(0)?,
                profile: row.get(1)?,
                status: row.get(2)?,
                error_count_1h: row.get(3)?,
                penalty: row.get(4)?,
                last_used_at: row.get(5)?,
            })
        },
    )
    .unwrap_or_else(|_| ProfileHealth {
        agent: agent.to_string(),
        profile: profile.to_string(),
        status: "healthy".to_string(),
        error_count_1h: 0,
        penalty: 0.0,
        last_used_at: None,
    })
}

pub fn list_health(conn: &Connection, agent: &str) -> Vec<ProfileHealth> {
    let mut stmt = conn
        .prepare(
            "SELECT agent, profile, status, error_count_1h, penalty, last_used_at FROM profile_health WHERE agent = ?1",
        )
        .unwrap();
    stmt.query_map(params![agent], |row| {
        Ok(ProfileHealth {
            agent: row.get(0)?,
            profile: row.get(1)?,
            status: row.get(2)?,
            error_count_1h: row.get(3)?,
            penalty: row.get(4)?,
            last_used_at: row.get(5)?,
        })
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

pub fn set_alias(conn: &Connection, agent: &str, alias: &str, profile: &str) {
    conn.execute(
        "INSERT INTO aliases (agent, alias, profile) VALUES (?1, ?2, ?3) ON CONFLICT(agent, alias) DO UPDATE SET profile = excluded.profile",
        params![agent, alias, profile],
    )
    .ok();
}

pub fn resolve_alias(conn: &Connection, agent: &str, name: &str) -> Option<String> {
    conn.query_row(
        "SELECT profile FROM aliases WHERE agent = ?1 AND alias = ?2",
        params![agent, name],
        |row| row.get(0),
    )
    .ok()
}

pub fn set_project(conn: &Connection, path: &str, agent: &str, profile: &str) {
    conn.execute(
        "INSERT INTO projects (path, agent, profile) VALUES (?1, ?2, ?3) ON CONFLICT(path, agent) DO UPDATE SET profile = excluded.profile",
        params![path, agent, profile],
    )
    .ok();
}

pub fn get_project(conn: &Connection, path: &str, agent: &str) -> Option<String> {
    let mut stmt = conn
        .prepare("SELECT path, profile FROM projects WHERE agent = ?1 ORDER BY length(path) DESC")
        .unwrap();
    let candidates: Vec<(String, String)> = stmt
        .query_map(params![agent], |row| {
            Ok((row.get::<_, String>(0)?, row.get(1)?))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    for (p, profile) in candidates {
        if path.starts_with(&p) {
            return Some(profile);
        }
    }
    None
}

pub fn unset_project(conn: &Connection, path: &str, agent: &str) {
    conn.execute(
        "DELETE FROM projects WHERE path = ?1 AND agent = ?2",
        params![path, agent],
    )
    .ok();
}

pub fn set_rotation_last(conn: &Connection, agent: &str, profile: &str, algorithm: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO rotation_state (agent, last_profile, algorithm) VALUES (?1, ?2, ?3)",
        params![agent, profile, algorithm],
    )
    .ok();
}

pub fn get_rotation_last(conn: &Connection, agent: &str) -> Option<(String, String)> {
    conn.query_row(
        "SELECT last_profile, algorithm FROM rotation_state WHERE agent = ?1",
        params![agent],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn open_or_rebuild_creates_tables() {
        with_temp_home(|| {
            let conn = open_or_rebuild();
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .unwrap();
            let tables: Vec<String> = stmt
                .query_map([], |row| row.get(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            assert!(tables.contains(&"profile_health".to_string()));
            assert!(tables.contains(&"cooldowns".to_string()));
            assert!(tables.contains(&"aliases".to_string()));
            assert!(tables.contains(&"projects".to_string()));
            assert!(tables.contains(&"rotation_state".to_string()));
        });
    }

    #[test]
    fn record_error_increments_count() {
        with_temp_home(|| {
            let conn = open_or_rebuild();
            record_error(&conn, "claude-code", "alice", "rate limit exceeded");
            let h = get_health(&conn, "claude-code", "alice");
            assert_eq!(h.error_count_1h, 1);
            assert!(h.penalty > 0.0);
        });
    }

    #[test]
    fn record_error_resets_after_hour() {
        with_temp_home(|| {
            let conn = open_or_rebuild();
            record_error(&conn, "claude-code", "alice", "timeout");
            conn.execute(
                "UPDATE profile_health SET last_error_time = datetime('now', '-2 hours') WHERE agent = ?1 AND profile = ?2",
                params!["claude-code", "alice"],
            )
            .unwrap();
            record_error(&conn, "claude-code", "alice", "timeout");
            let h = get_health(&conn, "claude-code", "alice");
            assert_eq!(h.error_count_1h, 1);
        });
    }

    #[test]
    fn set_cooldown_and_list() {
        with_temp_home(|| {
            let conn = open_or_rebuild();
            set_cooldown(&conn, "claude-code", "alice", 30, "manual");
            let list = list_cooldowns(&conn, Some("claude-code"));
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].profile, "alice");
        });
    }

    #[test]
    fn clear_cooldown_removes() {
        with_temp_home(|| {
            let conn = open_or_rebuild();
            set_cooldown(&conn, "claude-code", "alice", 30, "test");
            clear_cooldown(&conn, "claude-code", "alice");
            let list = list_cooldowns(&conn, Some("claude-code"));
            assert!(list.is_empty());
        });
    }

    #[test]
    fn alias_set_and_resolve() {
        with_temp_home(|| {
            let conn = open_or_rebuild();
            set_alias(&conn, "claude-code", "work", "work@company.com");
            assert_eq!(
                resolve_alias(&conn, "claude-code", "work"),
                Some("work@company.com".to_string())
            );
            assert_eq!(resolve_alias(&conn, "claude-code", "unknown"), None);
        });
    }

    #[test]
    fn project_association_cascading() {
        with_temp_home(|| {
            let conn = open_or_rebuild();
            set_project(&conn, "/home/user/projects", "claude-code", "work");
            assert_eq!(
                get_project(&conn, "/home/user/projects/sub", "claude-code"),
                Some("work".to_string())
            );
            unset_project(&conn, "/home/user/projects", "claude-code");
            assert_eq!(
                get_project(&conn, "/home/user/projects/sub", "claude-code"),
                None
            );
        });
    }

    #[test]
    fn rotation_last_tracks() {
        with_temp_home(|| {
            let conn = open_or_rebuild();
            set_rotation_last(&conn, "claude-code", "alice", "smart");
            let last = get_rotation_last(&conn, "claude-code");
            assert_eq!(
                last,
                Some(("alice".to_string(), "smart".to_string()))
            );
        });
    }
}
