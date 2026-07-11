use rusqlite::Connection;

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            project TEXT, directory TEXT,
            started_at TEXT NOT NULL, ended_at TEXT,
            summary TEXT, status TEXT DEFAULT 'active',
            task TEXT,
            findings TEXT DEFAULT '[]',
            decisions TEXT DEFAULT '[]',
            files_touched TEXT DEFAULT '[]',
            evidence TEXT DEFAULT '[]',
            stats TEXT DEFAULT '{}',
            compaction_snapshot TEXT,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS observations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT REFERENCES sessions(id),
            type TEXT NOT NULL,
            title TEXT NOT NULL, content TEXT NOT NULL,
            tool_name TEXT, project TEXT, scope TEXT DEFAULT 'project',
            topic_key TEXT, normalized_hash TEXT,
            revision_count INTEGER DEFAULT 0, duplicate_count INTEGER DEFAULT 0,
            last_seen_at TEXT, review_after TEXT, pinned INTEGER DEFAULT 0,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_obs_session ON observations(session_id);
        CREATE INDEX IF NOT EXISTS idx_obs_project ON observations(project);
        CREATE INDEX IF NOT EXISTS idx_obs_type ON observations(type);
        CREATE INDEX IF NOT EXISTS idx_obs_topic_key ON observations(topic_key);
        CREATE INDEX IF NOT EXISTS idx_obs_normalized_hash ON observations(normalized_hash);
        CREATE INDEX IF NOT EXISTS idx_obs_pinned ON observations(pinned) WHERE pinned = 1;
        CREATE INDEX IF NOT EXISTS idx_obs_review_after ON observations(review_after) WHERE review_after IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_obs_deleted ON observations(deleted_at) WHERE deleted_at IS NULL;

        CREATE VIRTUAL TABLE IF NOT EXISTS observations_fts
            USING fts5(title, content, tool_name, type, project, content='observations', content_rowid='id');

        CREATE TRIGGER IF NOT EXISTS obs_ai AFTER INSERT ON observations BEGIN
            INSERT INTO observations_fts(rowid, title, content, tool_name, type, project)
            VALUES (new.id, new.title, new.content, new.tool_name, new.type, new.project);
        END;

        CREATE TRIGGER IF NOT EXISTS obs_ad AFTER DELETE ON observations BEGIN
            INSERT INTO observations_fts(observations_fts, rowid, title, content, tool_name, type, project)
            VALUES('delete', old.id, old.title, old.content, old.tool_name, old.type, old.project);
        END;

        CREATE TRIGGER IF NOT EXISTS obs_au AFTER UPDATE ON observations BEGIN
            INSERT INTO observations_fts(observations_fts, rowid, title, content, tool_name, type, project)
            VALUES('delete', old.id, old.title, old.content, old.tool_name, old.type, old.project);
            INSERT INTO observations_fts(rowid, title, content, tool_name, type, project)
            VALUES (new.id, new.title, new.content, new.tool_name, new.type, new.project);
        END;

        CREATE TABLE IF NOT EXISTS user_prompts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT REFERENCES sessions(id),
            content TEXT NOT NULL, project TEXT, created_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_prompts_session ON user_prompts(session_id);
        CREATE INDEX IF NOT EXISTS idx_prompts_project ON user_prompts(project);

        CREATE VIRTUAL TABLE IF NOT EXISTS prompts_fts
            USING fts5(content, project, content='user_prompts', content_rowid='id');

        CREATE TRIGGER IF NOT EXISTS prompts_ai AFTER INSERT ON user_prompts BEGIN
            INSERT INTO prompts_fts(rowid, content, project) VALUES (new.id, new.content, new.project);
        END;

        CREATE TRIGGER IF NOT EXISTS prompts_ad AFTER DELETE ON user_prompts BEGIN
            INSERT INTO prompts_fts(prompts_fts, rowid, content, project)
            VALUES('delete', old.id, old.content, old.project);
        END;

        CREATE TRIGGER IF NOT EXISTS prompts_au AFTER UPDATE ON user_prompts BEGIN
            INSERT INTO prompts_fts(prompts_fts, rowid, content, project)
            VALUES('delete', old.id, old.content, old.project);
            INSERT INTO prompts_fts(rowid, content, project)
            VALUES (new.id, new.content, new.project);
        END;

        CREATE TABLE IF NOT EXISTS memory_relations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_id INTEGER NOT NULL REFERENCES observations(id),
            target_id INTEGER NOT NULL REFERENCES observations(id),
            relation TEXT NOT NULL,
            judgment_status TEXT NOT NULL DEFAULT 'pending',
            reason TEXT, evidence TEXT, confidence REAL,
            marked_by_actor TEXT, marked_by_kind TEXT, marked_by_model TEXT,
            session_id TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_rel_source ON memory_relations(source_id);
        CREATE INDEX IF NOT EXISTS idx_rel_target ON memory_relations(target_id);
        CREATE INDEX IF NOT EXISTS idx_rel_session ON memory_relations(session_id);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_rel_pair ON memory_relations(source_id, target_id, relation);

        CREATE TABLE IF NOT EXISTS session_summaries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            project TEXT NOT NULL, session_id TEXT REFERENCES sessions(id),
            seq INTEGER NOT NULL, summary TEXT NOT NULL, searchable_text TEXT NOT NULL,
            created_at TEXT NOT NULL, UNIQUE(project, seq)
        );

        CREATE INDEX IF NOT EXISTS idx_summaries_project ON session_summaries(project);
        CREATE INDEX IF NOT EXISTS idx_summaries_session ON session_summaries(session_id);
        ",
    )?;
    Ok(())
}
