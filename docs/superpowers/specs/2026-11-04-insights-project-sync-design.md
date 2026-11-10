# Project-scoped insights sync

## Intent

Make insights sync fast when an AI agent runs it from a project: ingest that
project's sessions rather than all local agent history. Manual CLI sync remains
global. This default was selected by the user.

## Approaches

- Filter the final parsed bundles: simplest, but still reads and parses unrelated
  sessions, so it does not address the requested performance improvement.
- Filter at each source before loading session bodies: recommended; reuse source
  project metadata and existing parsers. This is the selected implementation.
- Separate observatory database per project: unnecessary duplication; retain the
  existing shared insights database.

## Behavior and implementation

- Reuse `flare_process::agent_name()` to identify agent-run CLI syncs.
- Resolve the invoking directory to its Git worktree root, falling back to the
  invoking directory for non-Git projects. A failure to resolve the directory
  reports an error rather than silently importing all history.
- Match normalized project paths, including session directories below the root.
  Match full path components, so `project-other` does not match `project`.
- Select OpenCode sessions by directory metadata before loading messages/parts;
  do not let the global recent-session limit hide an eligible older session.
- Select Cursor composers by workspace metadata before loading bubbles.
- For JSONL sources, inspect project metadata before full transcript parsing;
  use Antigravity's native workspace mapping before opening transcripts, falling
  back to transcript hints for conversations absent from that mapping.
- Sessions with no usable project metadata are excluded from scoped syncs and
  remain available through a manual global sync.
- Keep cursor state separate for each normalized project and for global syncs.
  Syncing one project must never mark another project's history as imported.
- Preserve `--force` and keep pruning semantics unchanged.

## Verification

Test agent/manual scope selection, nested paths and similar path prefixes,
OpenCode and Cursor filtering before body reads, and project A/project B/global
syncs against the same source and destination. Verify failed reads/writes do not
advance the relevant cursor. Run insights tests and compile the CLI callers.

## Scope limits

This does not introduce row-level incremental ingestion, periodic jobs, new
dependencies, or a separate database for each project. Source metadata may still
need a lightweight scan to find eligible sessions.
