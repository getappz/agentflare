-- Table-rebuild pattern (SQLite has no ALTER TABLE ADD CONSTRAINT / no way to
-- widen a PRIMARY KEY in place) -- same shape as 0002_schema_constraints.sql.
-- Adds a `relation_type` column so the same ordered pair of items can hold
-- more than one relation independently (e.g. both `blocks` and
-- `relates_to`), and widens the primary key to include it. Every existing
-- row becomes `relation_type = 'blocks'` -- exactly its current implicit
-- meaning, so this is a pure additive migration with no semantic change to
-- existing data.
CREATE TABLE item_dependencies_new (
  item_id TEXT NOT NULL REFERENCES items(id),
  depends_on_item_id TEXT NOT NULL REFERENCES items(id),
  relation_type TEXT NOT NULL DEFAULT 'blocks' CHECK (relation_type IN ('blocks', 'duplicate', 'relates_to')),
  CHECK (item_id != depends_on_item_id),
  PRIMARY KEY (item_id, depends_on_item_id, relation_type)
);
INSERT INTO item_dependencies_new (item_id, depends_on_item_id, relation_type)
  SELECT item_id, depends_on_item_id, 'blocks' FROM item_dependencies;
DROP TABLE item_dependencies;
ALTER TABLE item_dependencies_new RENAME TO item_dependencies;
