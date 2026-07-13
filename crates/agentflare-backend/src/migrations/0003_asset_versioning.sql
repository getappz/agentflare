-- Each attach on the same (entity_type, entity_id, filename) is a new,
-- immutable row rather than an in-place update — version is just its
-- ordinal within that group, computed at insert time in asset::create.
ALTER TABLE assets ADD COLUMN version INTEGER NOT NULL DEFAULT 1;
