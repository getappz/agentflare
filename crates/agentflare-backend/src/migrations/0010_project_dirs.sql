CREATE TABLE IF NOT EXISTS project_dirs (
  project_id  TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  folder_path TEXT NOT NULL,
  updated_at  INTEGER NOT NULL
);
