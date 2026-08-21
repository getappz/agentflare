CREATE TABLE IF NOT EXISTS skills (
  name TEXT NOT NULL,
  source TEXT NOT NULL,
  path TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  tags TEXT NOT NULL DEFAULT '',
  est_tokens INTEGER NOT NULL DEFAULT 0,
  mtime INTEGER NOT NULL DEFAULT 0,
  shadow_path TEXT,
  PRIMARY KEY (name, source)
);
CREATE VIRTUAL TABLE IF NOT EXISTS skills_fts USING fts5(name, description, tags);
