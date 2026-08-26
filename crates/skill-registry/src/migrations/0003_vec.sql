CREATE TABLE IF NOT EXISTS skills_vec (
  name TEXT NOT NULL,
  source TEXT NOT NULL,
  embedding BLOB NOT NULL,
  dim INTEGER NOT NULL,
  model TEXT NOT NULL DEFAULT '',
  updated_at TEXT NOT NULL,
  PRIMARY KEY (name, source),
  FOREIGN KEY (name, source) REFERENCES skills(name, source) ON DELETE CASCADE
);
