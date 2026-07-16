#!/usr/bin/env bash
# scripts/pm-fixtures.sh — backdate updated_at on FIX-* items in the
# current agentflare project.  Run AFTER seeding FIX-01..FIX-10 via the
# `item` MCP tool.  All SQL is scoped to the current project ID and
# FIX-* names — never touches real items.
set -euo pipefail
DB="${AGENTFLARE_DB:-$HOME/.agentflare/backend.db}"
[ -f "$DB" ] || { echo "no DB at $DB" >&2; exit 1; }

# Resolve the project linked to the current repo (agentflare).
PID=$(sqlite3 "$DB" "
  SELECT p.id FROM projects p
  JOIN project_links l ON l.project_id = p.id
  WHERE l.repo_key = 'getappz/agentflare'
    AND p.deleted_at IS NULL
  LIMIT 1;
")
[ -n "$PID" ] || { echo "agentflare project not found — is the repo linked?" >&2; exit 1; }

# Guard: refuse if any FIX-* row lives outside this project (name collision).
STRAY=$(sqlite3 "$DB" "SELECT count(*) FROM items WHERE name LIKE 'FIX-%' AND project_id<>'$PID';")
[ "$STRAY" = "0" ] || { echo "refusing: $STRAY FIX-* items outside project $PID" >&2; exit 1; }

now=$(date +%s); day=86400
bd() { sqlite3 "$DB" "UPDATE items SET updated_at=$1 WHERE project_id='$PID' AND name IN ($2);"; }
bd $((now-25*day)) "'FIX-08','FIX-09'"   # stale backlog >14d
bd $((now-3*day))  "'FIX-05'"            # completed this week
bd $((now-10*day)) "'FIX-06'"            # completed prior week
bd $((now-20*day)) "'FIX-03'"            # started but stuck
echo "backdated FIX-* items in $DB (project $PID)"
