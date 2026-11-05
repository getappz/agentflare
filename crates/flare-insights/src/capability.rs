use serde::Serialize;

/// Adapter-level support, not a guarantee that every source event has a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    Recorded,
    Partial,
    NotRecorded,
}

#[derive(Debug, Serialize)]
pub struct AdapterCapabilities {
    pub source: &'static str,
    pub cache_tokens: Support,
    pub turn_timestamps: Support,
    pub tool_failure_status: Support,
    pub cost_basis: Support,
    pub project_path: Support,
    pub subagents: Support,
}

/// Parser capabilities from the F6 audit. Estimated pricing counts as recorded
/// cost support; it does not imply provider billing or complete pricing coverage.
pub fn capabilities() -> &'static [AdapterCapabilities] {
    use Support::{NotRecorded, Partial, Recorded};

    static CAPABILITIES: &[AdapterCapabilities] = &[
        AdapterCapabilities {
            source: "claude_code",
            cache_tokens: Recorded,
            turn_timestamps: Partial,
            tool_failure_status: Recorded,
            cost_basis: Recorded,
            // project_path is the log directory; cwd may identify the checkout.
            project_path: Partial,
            // Event rows and tool-name heuristics can duplicate the same agent.
            subagents: Partial,
        },
        AdapterCapabilities {
            source: "codex",
            cache_tokens: Recorded,
            turn_timestamps: Recorded,
            tool_failure_status: NotRecorded,
            cost_basis: NotRecorded,
            project_path: Recorded,
            subagents: NotRecorded,
        },
        AdapterCapabilities {
            source: "opencode",
            cache_tokens: Recorded,
            turn_timestamps: NotRecorded,
            tool_failure_status: Recorded,
            cost_basis: Recorded,
            // Some schemas supply only a project id rather than a directory.
            project_path: Partial,
            // Child links are capped and may also appear as standalone sessions.
            subagents: Partial,
        },
        AdapterCapabilities {
            source: "cursor",
            cache_tokens: NotRecorded,
            turn_timestamps: Recorded,
            tool_failure_status: Recorded,
            cost_basis: Recorded,
            project_path: Recorded,
            subagents: Recorded,
        },
        AdapterCapabilities {
            source: "antigravity",
            cache_tokens: Recorded,
            turn_timestamps: Recorded,
            // Failure is inferred from command-exit text in tool output.
            tool_failure_status: Partial,
            cost_basis: Recorded,
            project_path: Partial,
            // Only tool names containing subagent are recognized.
            subagents: Partial,
        },
    ];
    CAPABILITIES
}

pub fn for_source(name: &str) -> Option<&'static AdapterCapabilities> {
    capabilities().iter().find(|row| row.source == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::IngestManager;

    #[test]
    fn every_registered_adapter_has_a_capability_row() {
        let sources = IngestManager::new().source_names();
        assert_eq!(capabilities().len(), sources.len());
        for source in &sources {
            assert_eq!(
                capabilities()
                    .iter()
                    .filter(|row| row.source == *source)
                    .count(),
                1,
                "expected exactly one capability row for {source}"
            );
            assert_eq!(for_source(source).unwrap().source, *source);
        }
        for row in capabilities() {
            assert!(
                sources.contains(&row.source),
                "unregistered source {}",
                row.source
            );
        }
        assert!(for_source("unknown").is_none());
    }

    #[test]
    fn serialized_capabilities_expose_audited_limits() {
        let rows = serde_json::to_value(capabilities()).unwrap();
        let row = |source| {
            rows.as_array()
                .unwrap()
                .iter()
                .find(|row| row["source"] == source)
                .unwrap()
        };
        assert_eq!(row("claude_code")["cache_tokens"], "recorded");
        assert_eq!(row("claude_code")["tool_failure_status"], "recorded");
        assert_eq!(row("claude_code")["turn_timestamps"], "partial");
        assert_eq!(row("codex")["cache_tokens"], "recorded");
        assert_eq!(row("codex")["tool_failure_status"], "not_recorded");
        assert_eq!(row("codex")["cost_basis"], "not_recorded");
        assert_eq!(row("codex")["subagents"], "not_recorded");
        assert_eq!(row("opencode")["turn_timestamps"], "not_recorded");
        assert_eq!(row("cursor")["cache_tokens"], "not_recorded");
        assert_eq!(row("cursor")["tool_failure_status"], "recorded");
        assert_eq!(row("cursor")["turn_timestamps"], "recorded");
        assert_eq!(row("antigravity")["tool_failure_status"], "partial");
        assert_eq!(row("antigravity")["project_path"], "partial");
    }
}
