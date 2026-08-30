CREATE TABLE IF NOT EXISTS tools (
  server TEXT NOT NULL,
  name TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  input_schema TEXT NOT NULL DEFAULT '{}',
  PRIMARY KEY (server, name)
);
CREATE VIRTUAL TABLE IF NOT EXISTS tools_fts USING fts5(server, name, description);
