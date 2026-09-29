# Sandbox limits

Safety ceilings for `flare-sandbox` + `agentflare-jobs` sandbox paths,
ported from OpenShell's `sandbox-limits.md`. Limits are safety boundaries,
not capacity targets. Every buffer, queue, table, and wait states its owner,
scope, bound, and terminal behavior below.

## Limit model

- Enforce the bound before allocation or admission where possible.
- Operator/config may narrow a ceiling, never raise it silently.
- `fail-open` (fallback unsandboxed) never bypasses a bound; under
  `FLARE_SANDBOX_FAIL_CLOSED` the fallback becomes an error instead.

## Bounds

| Resource | Bound | Scope / terminal behavior |
|---|---:|---|
| Diagnostic-log snapshot | 8 KiB (`DIAGNOSTIC_TAIL_BYTES`) | Per snapshot, every 3 s while the job runs. A runaway log never becomes a second copy of the session. |
| Job stdout/stderr log | 64 MiB per stream (`MAX_LOG_STREAM_BYTES`) | Per job stream. Past the cap the pump keeps draining (child never blocks on a full pipe) but stops writing; one `[job log truncated …]` marker appended. |
| Binary identity pins | 4,096 entries (`MAX_IDENTITY_PINS`) | Per process lifetime. Past the cap, profile matching degrades to basename-only with an `identity_pins_exhausted` event; existing pins are never evicted. |
| Denial summary | 64 distinct denials (`MAX_SUMMARIZED_DENIALS`) | Per advisory pass. Extra denials ignored; counts saturate instead of wrapping. |
| Policy proposal output | One line per proposal (`render_advice`) | Bounded by the denial summary above. No bodies, credentials, or prompt text in any line. |

## Telemetry

All sandbox events (`fallback_unsandboxed`, `skipped_mount`,
`identity_mismatch`, `identity_pins_exhausted`, `invalid_writable_dir`)
are single stderr lines carrying only command, path, and fixed-vocabulary
reason -- never file contents, credentials, or prompt text.

## Review triggers

Revisit this file when adding a parser, buffer, queue, cache, retry loop,
or external call to these crates. State: what untrusted resource can grow,
who owns the bound, per-what scope, whether config can narrow it, how
saturation terminates, and which test proves it.
