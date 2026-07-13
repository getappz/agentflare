CREATE TABLE IF NOT EXISTS workspaces (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  slug TEXT NOT NULL,
  owner_agent TEXT,
  item_label TEXT NOT NULL DEFAULT 'Item',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  deleted_at INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_workspaces_slug ON workspaces(slug) WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS projects (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  name TEXT NOT NULL,
  identifier TEXT NOT NULL,
  archived_at INTEGER,
  external_source TEXT,
  external_id TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  deleted_at INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_name ON projects(name, workspace_id) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_identifier ON projects(identifier, workspace_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_projects_workspace ON projects(workspace_id) WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS project_sequences (
  project_id TEXT PRIMARY KEY REFERENCES projects(id),
  next_seq INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS states (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  name TEXT NOT NULL,
  group_name TEXT NOT NULL,
  sequence REAL NOT NULL,
  is_default INTEGER NOT NULL DEFAULT 0,
  color TEXT NOT NULL DEFAULT '#60646C',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  deleted_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_states_project ON states(project_id) WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS items (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  state_id TEXT NOT NULL REFERENCES states(id),
  name TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  priority TEXT NOT NULL DEFAULT 'none',
  parent_id TEXT REFERENCES items(id),
  assignee_agent TEXT,
  sequence_id INTEGER NOT NULL DEFAULT 1,
  sort_order REAL NOT NULL DEFAULT 65535,
  started_at INTEGER,
  completed_at INTEGER,
  archived_at INTEGER,
  external_source TEXT,
  external_id TEXT,
  metadata TEXT NOT NULL DEFAULT '{}',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  deleted_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_items_project ON items(project_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_items_state ON items(state_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_items_assignee ON items(assignee_agent) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_items_parent ON items(parent_id) WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS labels (
  id TEXT PRIMARY KEY,
  project_id TEXT,
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
CREATE UNIQUE INDEX IF NOT EXISTS idx_labels_name_project ON labels(name, project_id) WHERE deleted_at IS NULL AND project_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_labels_name_workspace_only ON labels(name, workspace_id) WHERE deleted_at IS NULL AND project_id IS NULL;

CREATE TABLE IF NOT EXISTS item_labels (
  item_id TEXT NOT NULL REFERENCES items(id),
  label_id TEXT NOT NULL REFERENCES labels(id),
  PRIMARY KEY (item_id, label_id)
);

CREATE TABLE IF NOT EXISTS item_assignees (
  item_id TEXT NOT NULL REFERENCES items(id),
  agent_id TEXT NOT NULL,
  PRIMARY KEY (item_id, agent_id)
);

CREATE TABLE IF NOT EXISTS item_dependencies (
  item_id TEXT NOT NULL REFERENCES items(id),
  depends_on_item_id TEXT NOT NULL REFERENCES items(id),
  PRIMARY KEY (item_id, depends_on_item_id)
);

CREATE TABLE IF NOT EXISTS webhooks (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  url TEXT NOT NULL,
  is_active INTEGER NOT NULL DEFAULT 1,
  secret_key TEXT NOT NULL,
  on_item INTEGER NOT NULL DEFAULT 0,
  on_state INTEGER NOT NULL DEFAULT 0,
  on_project INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  deleted_at INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_webhooks_url ON webhooks(workspace_id, url) WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS assets (
  id TEXT PRIMARY KEY,
  workspace_id TEXT,
  entity_type TEXT NOT NULL,
  entity_id TEXT NOT NULL,
  filename TEXT NOT NULL,
  size INTEGER NOT NULL DEFAULT 0,
  storage_path TEXT NOT NULL,
  mime_type TEXT,
  metadata TEXT NOT NULL DEFAULT '{}',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  deleted_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_assets_entity ON assets(entity_type, entity_id) WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS webhook_logs (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  webhook_id TEXT NOT NULL,
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
CREATE INDEX IF NOT EXISTS idx_webhook_logs_webhook ON webhook_logs(webhook_id);
CREATE INDEX IF NOT EXISTS idx_webhook_logs_workspace ON webhook_logs(workspace_id);
