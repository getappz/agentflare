ALTER TABLE vents ADD COLUMN escalation_state TEXT NOT NULL DEFAULT 'none';
ALTER TABLE vents ADD COLUMN escalation_level INTEGER NOT NULL DEFAULT 0;
ALTER TABLE vents ADD COLUMN escalated_at INTEGER;
ALTER TABLE vents ADD COLUMN acknowledged_at INTEGER;
ALTER TABLE vents ADD COLUMN resolved_at INTEGER;
CREATE INDEX IF NOT EXISTS idx_vents_escalation_state ON vents(project_id, escalation_state);
