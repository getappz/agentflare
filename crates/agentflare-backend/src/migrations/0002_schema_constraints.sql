-- Rebuilding tables below (SQLite has no ALTER TABLE ADD CONSTRAINT). Each
-- rebuild creates the new table under a "_new" name, copies into it, drops
-- the original, then renames "_new" into the original's name — never the
-- other way around. Renaming a table *away* from its original name would
-- silently rewrite other tables' existing FK definitions that reference it
-- by that name (e.g. item_labels.label_id REFERENCES labels(id)) to point at
-- the new name instead; going new-name-first sidesteps that entirely since
-- nothing ever references the "_new" name.

-- Reject self-referencing dependencies. Any pre-existing violation (there is
-- no app-level guard before this migration — item::add_dependency does a raw
-- INSERT OR IGNORE) is dropped during the copy rather than failing the
-- migration.
CREATE TABLE item_dependencies_new (
  item_id TEXT NOT NULL REFERENCES items(id),
  depends_on_item_id TEXT NOT NULL REFERENCES items(id),
  CHECK (item_id != depends_on_item_id),
  PRIMARY KEY (item_id, depends_on_item_id)
);
INSERT INTO item_dependencies_new (item_id, depends_on_item_id)
  SELECT item_id, depends_on_item_id FROM item_dependencies
  WHERE item_id != depends_on_item_id;
DROP TABLE item_dependencies;
ALTER TABLE item_dependencies_new RENAME TO item_dependencies;

-- Enforce labels.project_id -> projects(id) (nullable: NULL means a
-- workspace-only label).
CREATE TABLE labels_new (
  id TEXT PRIMARY KEY,
  project_id TEXT REFERENCES projects(id),
  workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  name TEXT NOT NULL,
  color TEXT NOT NULL DEFAULT '#60646C',
  parent_id TEXT REFERENCES labels(id),
  sort_order REAL NOT NULL DEFAULT 65535,
  external_source TEXT,
  external_id TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  deleted_at INTEGER
);
INSERT INTO labels_new (id, project_id, workspace_id, name, color, parent_id,
                         sort_order, external_source, external_id, created_at,
                         updated_at, deleted_at)
  SELECT id, project_id, workspace_id, name, color, parent_id, sort_order,
         external_source, external_id, created_at, updated_at, deleted_at
  FROM labels;
DROP TABLE labels;
ALTER TABLE labels_new RENAME TO labels;
CREATE UNIQUE INDEX IF NOT EXISTS idx_labels_name_project ON labels(name, project_id) WHERE deleted_at IS NULL AND project_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_labels_name_workspace_only ON labels(name, workspace_id) WHERE deleted_at IS NULL AND project_id IS NULL;

-- Enforce webhook_logs.webhook_id -> webhooks(id), and add an index on
-- created_at for future retention-purge queries (raw request/response
-- bodies here can carry secrets/PII forwarded through webhooks and
-- currently have no TTL — actual scheduled purging is a separate,
-- not-yet-requested piece of work; this just makes it cheap to add).
CREATE TABLE webhook_logs_new (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  webhook_id TEXT NOT NULL REFERENCES webhooks(id),
  event_type TEXT,
  request_method TEXT,
  request_headers TEXT,
  request_body TEXT,
  response_status TEXT,
  response_headers TEXT,
  response_body TEXT,
  retry_count INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);
INSERT INTO webhook_logs_new (id, workspace_id, webhook_id, event_type,
                               request_method, request_headers, request_body,
                               response_status, response_headers,
                               response_body, retry_count, created_at)
  SELECT id, workspace_id, webhook_id, event_type, request_method,
         request_headers, request_body, response_status, response_headers,
         response_body, retry_count, created_at
  FROM webhook_logs;
DROP TABLE webhook_logs;
ALTER TABLE webhook_logs_new RENAME TO webhook_logs;
CREATE INDEX IF NOT EXISTS idx_webhook_logs_webhook ON webhook_logs(webhook_id);
CREATE INDEX IF NOT EXISTS idx_webhook_logs_workspace ON webhook_logs(workspace_id);
CREATE INDEX IF NOT EXISTS idx_webhook_logs_created_at ON webhook_logs(created_at);
