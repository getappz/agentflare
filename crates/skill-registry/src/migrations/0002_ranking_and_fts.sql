CREATE TABLE IF NOT EXISTS skill_impressions (
  name TEXT NOT NULL,
  source TEXT NOT NULL,
  surfaced_at INTEGER NOT NULL,
  PRIMARY KEY (name, source)
);
