CREATE TABLE IF NOT EXISTS bridge_repos (
  repo        TEXT PRIMARY KEY,
  project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  folder_path TEXT NOT NULL,
  queue_label TEXT NOT NULL,
  work_agent  TEXT,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);
