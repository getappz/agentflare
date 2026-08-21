DROP TABLE IF EXISTS tools_fts;
DROP TRIGGER IF EXISTS tools_fts_ai;
DROP TRIGGER IF EXISTS tools_fts_ad;
DROP TRIGGER IF EXISTS tools_fts_au;
CREATE VIRTUAL TABLE tools_fts USING fts5(
  server, name, description, content='tools'
);
CREATE TRIGGER tools_fts_ai AFTER INSERT ON tools BEGIN
  INSERT INTO tools_fts(rowid, server, name, description)
  VALUES (new.rowid, new.server, new.name, new.description);
END;
CREATE TRIGGER tools_fts_ad AFTER DELETE ON tools BEGIN
  INSERT INTO tools_fts(tools_fts, rowid, server, name, description)
  VALUES ('delete', old.rowid, old.server, old.name, old.description);
END;
CREATE TRIGGER tools_fts_au
AFTER UPDATE OF server, name, description ON tools BEGIN
  INSERT INTO tools_fts(tools_fts, rowid, server, name, description)
  VALUES ('delete', old.rowid, old.server, old.name, old.description);
  INSERT INTO tools_fts(rowid, server, name, description)
  VALUES (new.rowid, new.server, new.name, new.description);
END;
INSERT INTO tools_fts(rowid, server, name, description)
SELECT rowid, server, name, description FROM tools;
