-- Append-only log of item assignee transitions (claims, handoffs, manual
-- reassignments). Written by item::update whenever assignee_agent actually
-- changes; read by the health scorecard's bottleneck signal ("items handed
-- between agents repeatedly"). History starts at the upgrade that ships this
-- table — transitions before it are unrecorded.
CREATE TABLE IF NOT EXISTS item_assignment_events (
  id         TEXT PRIMARY KEY,
  item_id    TEXT NOT NULL REFERENCES items(id),
  from_owner TEXT,
  to_owner   TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_item_assignment_events_item_time
  ON item_assignment_events (item_id, created_at);
