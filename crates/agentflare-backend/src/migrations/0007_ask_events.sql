CREATE TABLE IF NOT EXISTS ask_events (
  id            TEXT PRIMARY KEY,
  project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  goal_item_id  TEXT REFERENCES items(id) ON DELETE SET NULL,
  item_id       TEXT NOT NULL,
  agent         TEXT,
  reason        TEXT NOT NULL,
  gate_question TEXT,
  created_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ask_events_project_created ON ask_events(project_id, created_at);
