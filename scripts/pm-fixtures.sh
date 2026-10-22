#!/usr/bin/env bash
# scripts/pm-fixtures.sh — backdate updated_at on the FIX-01..FIX-10 fixture
# items so the PM skills (/pm:groom, /pm:standup, /pm:health) have stale,
# completed and stuck rows to report on.  All SQL is scoped to ONE project id
# (AGENTFLARE_PM_FIXTURES_PROJECT) and to FIX-* names — never real items.
#
# NEVER seed the fixtures from the leanstack checkout.  The `item` MCP tool
# writes to whichever project the *current repo* is linked to, so seeding from
# this repo puts FIX-01..FIX-10 into the live `agentflare` project.  That
# happened in July 2026: the fixtures polluted groom/standup/health for three
# months, two counted as completed work, alice/bob showed as stuck WIP, and
# the urgent FIX-07 was auto-dispatched to a real session.  This script now
# refuses to run without an explicit fixtures project and refuses the live one,
# whether it is linked as `git:getappz/agentflare` or by `path:` to any
# leanstack checkout/worktree (resolved through that directory's origin remote).
# A name shared by several projects (names are unique per workspace only) is
# refused too; pass the project id instead.
#
# Seed sequence — follow it exactly:
#   1. Create a scratch git repo.  agentflare links one project per repo
#      (projects.external_id = "git:<owner>/<repo>" or "path:<dir>"); the
#      project is named after the directory, so this one becomes `pm-fixtures`:
#        mkdir -p ~/scratch/pm-fixtures && cd ~/scratch/pm-fixtures && git init
#   2. From THAT directory (not leanstack), open an agentflare MCP session,
#      e.g. `claude`, and create FIX-01..FIX-10 with the `item` tool
#      (action=create, name/priority/state/assignee as the names describe).
#      The first call creates and links the `pm-fixtures` project.  Do NOT add
#      the `ready-for-work` label or the daemon will dispatch the fixtures.
#   3. Backdate, naming the fixtures project explicitly (name or UUID):
#        AGENTFLARE_PM_FIXTURES_PROJECT=pm-fixtures scripts/pm-fixtures.sh
#   4. Run the PM skills from the scratch directory to inspect their output.
#   5. Afterwards delete the fixtures (`item` action=delete) or archive the
#      project (`agentflare project archive pm-fixtures`) so they stay out of
#      every roll-up.
set -euo pipefail
DB="${AGENTFLARE_DB:-$HOME/.agentflare/backend.db}"
[ -f "$DB" ] || { echo "no DB at $DB" >&2; exit 1; }

LIVE_REPO_KEY="getappz/agentflare"

# Required: the dedicated fixtures project (name or id). Never defaults to the
# project linked to the current repo — that is how the live project got seeded.
FIXTURES="${AGENTFLARE_PM_FIXTURES_PROJECT:-}"
[ -n "$FIXTURES" ] || {
  echo "AGENTFLARE_PM_FIXTURES_PROJECT is unset: name the dedicated fixtures project (name or id); see the header of $0" >&2
  exit 1
}
FIXTURES_SQL=${FIXTURES//\'/\'\'}  # escape single quotes for the SQL literal
# Project names are unique per workspace only, so a name can match several
# live projects; refuse the ambiguity instead of picking one with LIMIT 1.
MATCHES=$(sqlite3 "$DB" "
  SELECT id FROM projects
  WHERE deleted_at IS NULL
    AND (id = '$FIXTURES_SQL' OR lower(name) = lower('$FIXTURES_SQL'))
  ORDER BY id;
")
case "$MATCHES" in
  "") echo "fixtures project '$FIXTURES' not found (name or id, not soft-deleted)" >&2; exit 1 ;;
  *$'\n'*) echo "fixtures project '$FIXTURES' matches several projects; use its id" >&2; exit 1 ;;
esac
PID=$MATCHES

# Refuse the live project.  Its link lives in projects.external_id as
# "git:getappz/agentflare" (pre-prefix rows hold the bare key), but a project
# can also be linked by "path:<dir>" to a leanstack checkout or worktree, so a
# path link is resolved through that directory's origin remote.
REPO_KEY=$(sqlite3 "$DB" "SELECT coalesce(external_id, '') FROM projects WHERE id = '$PID';")
refuse_live() {
  echo "refusing: live project '$FIXTURES' ($PID, $REPO_KEY) is linked to $LIVE_REPO_KEY — point AGENTFLARE_PM_FIXTURES_PROJECT at a dedicated fixtures project" >&2
  exit 1
}
case "$REPO_KEY" in
  "$LIVE_REPO_KEY" | "git:$LIVE_REPO_KEY") refuse_live ;;
  path:*)
    LINK_DIR=${REPO_KEY#path:}
    EXT_PREFIX="\\\\?\\"  # Windows extended-length prefix (\\?\), which git rejects
    LINK_DIR=${LINK_DIR#"$EXT_PREFIX"}
    REMOTE=$(git -C "$LINK_DIR" remote get-url origin 2>/dev/null || true)
    REMOTE=${REMOTE%/}; REMOTE=${REMOTE%.git}
    case "$REMOTE" in *[/:]"$LIVE_REPO_KEY") refuse_live ;; esac ;;
esac

# Guard: refuse if any live FIX-* row exists outside this project (name
# collision).  Soft-deleted rows (the 2026 clean-up) are ignored.
STRAY=$(sqlite3 "$DB" "SELECT count(*) FROM items WHERE name LIKE 'FIX-%' AND deleted_at IS NULL AND project_id <> '$PID';")
[ "$STRAY" = "0" ] || { echo "refusing: $STRAY FIX-* items outside project $PID" >&2; exit 1; }

now=$(date +%s); day=86400
# Match on the FIX-NN prefix: fixtures are usually named "FIX-NN <scenario>".
bd() { sqlite3 "$DB" "UPDATE items SET updated_at=$1 WHERE project_id='$PID' AND deleted_at IS NULL AND substr(name, 1, 6) IN ($2);"; }
bd $((now-25*day)) "'FIX-08','FIX-09'"   # stale backlog >14d
bd $((now-3*day))  "'FIX-05'"            # completed this week
bd $((now-10*day)) "'FIX-06'"            # completed prior week
bd $((now-20*day)) "'FIX-03'"            # started but stuck
echo "backdated FIX-* items in $DB (project $PID)"
