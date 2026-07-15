//! Shadow-preferred skill loading and the Registry facade.

use crate::search::{MatchMode, SkillHit};
use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
pub struct LoadedSkill {
    pub name: String,
    pub source: String,
    pub path: PathBuf,
    pub compressed: bool,
    pub body: String,
    pub siblings: Vec<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("skill '{0}' not found — try skill_search with it as the query")]
    NotFound(String),
    #[error("ambiguous skill name; qualify as one of: {}", .0.join(", "))]
    Ambiguous(Vec<String>),
    #[error("io: {0}")]
    Io(String),
    #[error("db: {0}")]
    Db(String),
}

fn row_to_parts(r: &rusqlite::Row) -> rusqlite::Result<(String, String, String, Option<String>)> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
}

pub fn load(conn: &Connection, name: &str, original: bool) -> Result<LoadedSkill, LoadError> {
    // Qualified form is "<source>:<name>"; source ids may contain ':'
    // (claude-plugin:cv), so split on the LAST ':' and treat the left part
    // as the source. Fall back to bare-name lookup when no row matches.
    let db = |e: rusqlite::Error| LoadError::Db(e.to_string());
    let mut candidates: Vec<(String, String, String, Option<String>)> = Vec::new();
    if let Some((src, bare)) = name.rsplit_once(':') {
        let row = conn
            .query_row(
                "SELECT name, source, path, shadow_path FROM skills WHERE name = ?1 AND source = ?2",
                rusqlite::params![bare, src],
                row_to_parts,
            )
            .optional()
            .map_err(db)?;
        if let Some(r) = row {
            candidates.push(r);
        }
    }
    if candidates.is_empty() {
        let mut stmt = conn
            .prepare("SELECT name, source, path, shadow_path FROM skills WHERE name = ?1")
            .map_err(db)?;
        let rows = stmt
            .query_map([name], row_to_parts)
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        candidates = rows;
    }
    match candidates.len() {
        0 => Err(LoadError::NotFound(name.to_string())),
        1 => {
            let (name, source, path, shadow) = candidates.remove(0);
            let use_shadow = !original && shadow.is_some();
            let body_path = if use_shadow {
                PathBuf::from(shadow.clone().unwrap())
            } else {
                PathBuf::from(&path)
            };
            let body = std::fs::read_to_string(&body_path)
                .map_err(|e| LoadError::Io(format!("{}: {e}", body_path.display())))?;
            let dir = body_path.parent().unwrap_or(Path::new("."));
            let mut siblings = Vec::new();
            if let Ok(read) = std::fs::read_dir(dir) {
                for f in read.flatten() {
                    let p = f.path();
                    if p.is_file() && p.file_name().map(|n| n != "SKILL.md").unwrap_or(false) {
                        siblings.push(p);
                    }
                }
            }
            Ok(LoadedSkill {
                name,
                source,
                path: body_path,
                compressed: use_shadow,
                body,
                siblings,
            })
        }
        _ => Err(LoadError::Ambiguous(
            candidates
                .into_iter()
                .map(|(n, s, ..)| format!("{s}:{n}"))
                .collect(),
        )),
    }
}

/// Facade owning the connection + refresh debounce.
pub struct Registry {
    conn: Connection,
    detected_agents: Vec<String>,
    last_refresh: std::time::Instant,
    refreshed_once: bool,
}

pub const REFRESH_DEBOUNCE_SECS: u64 = 60;

impl Registry {
    pub fn open_default(db_path: &Path, detected_agents: Vec<String>) -> Result<Self, LoadError> {
        let conn = crate::db::open_db(db_path).map_err(|e| LoadError::Db(e.to_string()))?;
        Ok(Registry {
            conn,
            detected_agents,
            last_refresh: std::time::Instant::now(),
            refreshed_once: false,
        })
    }

    /// Rescan sources when never scanned or debounce elapsed.
    pub fn ensure_fresh(&mut self) -> Result<(), LoadError> {
        if self.refreshed_once && self.last_refresh.elapsed().as_secs() < REFRESH_DEBOUNCE_SECS {
            return Ok(());
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let sources = crate::sources::default_sources(&home, &cwd, &self.detected_agents);
        let out = crate::sources::scan_sources(&sources);
        crate::db::rebuild(&mut self.conn, &out.entries)
            .map_err(|e| LoadError::Db(e.to_string()))?;
        self.last_refresh = std::time::Instant::now();
        self.refreshed_once = true;
        Ok(())
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
        mode: MatchMode,
    ) -> Result<Vec<SkillHit>, LoadError> {
        crate::search::search(&self.conn, query, limit, mode)
            .map_err(|e| LoadError::Db(e.to_string()))
    }

    /// Every distinct skill name currently indexed, regardless of source.
    pub fn list_all_names(&self) -> Result<Vec<String>, LoadError> {
        crate::search::list_all_names(&self.conn).map_err(|e| LoadError::Db(e.to_string()))
    }

    pub fn load(&self, name: &str, original: bool) -> Result<LoadedSkill, LoadError> {
        load(&self.conn, name, original)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{open_in_memory, rebuild};
    use crate::sources::SkillEntry;
    use std::fs;

    fn seed_with_files() -> (tempfile::TempDir, Connection) {
        let tmp = tempfile::tempdir().unwrap();
        let orig_dir = tmp.path().join("plug").join("live");
        fs::create_dir_all(&orig_dir).unwrap();
        fs::write(orig_dir.join("SKILL.md"), "ORIGINAL BODY").unwrap();
        fs::write(orig_dir.join("ref.md"), "sibling").unwrap();
        let shadow_dir = tmp.path().join("user").join("live");
        fs::create_dir_all(&shadow_dir).unwrap();
        fs::write(shadow_dir.join("SKILL.md"), "COMPRESSED BODY").unwrap();

        let mut conn = open_in_memory().unwrap();
        let entries = vec![
            SkillEntry {
                name: "live".into(),
                source: "claude-plugin:cv".into(),
                path: orig_dir.join("SKILL.md"),
                description: "d".into(),
                tags: String::new(),
                est_tokens: 10,
                mtime: 1,
                shadow_path: Some(shadow_dir.join("SKILL.md")),
            },
            SkillEntry {
                name: "live".into(),
                source: "codex".into(),
                path: tmp.path().join("codex-live-SKILL.md"),
                description: "other agent's live".into(),
                tags: String::new(),
                est_tokens: 10,
                mtime: 1,
                shadow_path: None,
            },
        ];
        fs::write(tmp.path().join("codex-live-SKILL.md"), "CODEX BODY").unwrap();
        rebuild(&mut conn, &entries).unwrap();
        (tmp, conn)
    }

    #[test]
    fn bare_ambiguous_name_errors_with_qualified_candidates() {
        let (_tmp, conn) = seed_with_files();
        match load(&conn, "live", false) {
            Err(LoadError::Ambiguous(c)) => {
                assert!(c.contains(&"claude-plugin:cv:live".to_string()));
                assert!(c.contains(&"codex:live".to_string()));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn qualified_name_loads_shadow_by_default() {
        let (_tmp, conn) = seed_with_files();
        let s = load(&conn, "claude-plugin:cv:live", false).unwrap();
        assert!(s.compressed);
        assert_eq!(s.body, "COMPRESSED BODY");
    }

    #[test]
    fn original_flag_loads_source_body_and_siblings() {
        let (_tmp, conn) = seed_with_files();
        let s = load(&conn, "claude-plugin:cv:live", true).unwrap();
        assert!(!s.compressed);
        assert_eq!(s.body, "ORIGINAL BODY");
        assert_eq!(s.siblings.len(), 1);
        assert!(s.siblings[0].ends_with("ref.md"));
    }

    #[test]
    fn unknown_name_is_not_found() {
        let (_tmp, conn) = seed_with_files();
        assert!(matches!(
            load(&conn, "nope", false),
            Err(LoadError::NotFound(_))
        ));
    }
}
