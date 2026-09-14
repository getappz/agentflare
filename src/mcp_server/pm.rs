//! `pm` MCP tool: a thin, first-class dispatcher over the read-only
//! reporting actions the `pm` skill already teaches an agent to compose by
//! hand (`item` groom/standup/health, `project` list), plus a way to flip
//! PM mode that doesn't depend on a Claude Code hook matching literal
//! prompt text. Exists so a non-Claude-Code MCP client -- which never loads
//! `.claude/skills/pm/SKILL.md` and never runs agentflare's own
//! UserPromptSubmit hook -- still has a real tool call for every one of
//! that skill's five reports and its mode toggle. Never duplicates report
//! logic: every branch below delegates straight to the same `item_*`/
//! `project_inner` methods the skill itself calls, so the two paths can't
//! drift apart.
//!
//! Scoring/prioritization frameworks (RICE/WSJF/etc.) stay the caller's
//! job, same as for the skill -- the server only returns the precomputed
//! shortlist/buckets, never a judgment call it can't make from item data
//! alone.

use super::*;

impl AgentflareMcp {
    pub(super) fn pm_inner(&self, req: PmRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "standup" => self.item_standup(ItemRequest {
                action: "standup".into(),
                project: req.project,
                cutoff_hours: req.cutoff_hours,
                staleness_days: req.staleness_days,
                ..Default::default()
            }),
            "groom" => self.item_groom(ItemRequest {
                action: "groom".into(),
                state_group: Some("backlog,unstarted".into()),
                project: req.project,
                staleness_days: req.staleness_days,
                limit: Some(req.limit.unwrap_or(15)),
                ..Default::default()
            }),
            "plan" => self.item_groom(ItemRequest {
                action: "groom".into(),
                state_group: Some("backlog,unstarted".into()),
                project: req.project,
                staleness_days: req.staleness_days,
                capacity: Some(req.capacity.unwrap_or(5)),
                ..Default::default()
            }),
            "health" => self.item_health(ItemRequest {
                action: "health".into(),
                project: req.project,
                window_weeks: req.window_weeks,
                ..Default::default()
            }),
            "portfolio" => self.pm_portfolio(&req),
            "mode_on" => {
                crate::pm_mode::set_active().map_err(|e| {
                    ErrorData::internal_error(format!("failed to activate PM mode: {e}"), None)
                })?;
                Ok(r#"{"active":true}"#.to_string())
            }
            "mode_off" => {
                crate::pm_mode::clear_active();
                Ok(r#"{"active":false}"#.to_string())
            }
            "mode_status" => Ok(format!(r#"{{"active":{}}}"#, crate::pm_mode::is_active())),
            other => Err(ErrorData::invalid_params(
                format!(
                    "unknown pm action: '{other}' — expected standup|groom|plan|health|portfolio|mode_on|mode_off|mode_status"
                ),
                None,
            )),
        }
    }

    /// `portfolio`: loop every project in the workspace and roll up the
    /// same `health` (default) or `standup` report a direct `pm`/`item`
    /// call would run for one project -- reuses `project_inner`'s own
    /// "list" branch and `item_health`/`item_standup` rather than
    /// re-deriving either, so this can never see a different project set
    /// or a different report shape than calling those directly,
    /// project by project, would.
    fn pm_portfolio(&self, req: &PmRequest) -> Result<String, ErrorData> {
        let report = req.report.as_deref().unwrap_or("health");
        if report != "health" && report != "standup" {
            return Err(ErrorData::invalid_params(
                format!("unknown pm portfolio report: '{report}' — expected health|standup"),
                None,
            ));
        }
        let projects_json = self.project_inner(ProjectRequest {
            action: "list".into(),
        })?;
        let projects: Vec<agentflare_backend::project::Project> =
            serde_json::from_str(&projects_json).map_err(|e| {
                ErrorData::internal_error(format!("failed to parse project list: {e}"), None)
            })?;
        let mut rows = Vec::with_capacity(projects.len());
        for project in &projects {
            let item_req = ItemRequest {
                action: report.to_string(),
                project: Some(project.id.clone()),
                cutoff_hours: req.cutoff_hours,
                staleness_days: req.staleness_days,
                window_weeks: req.window_weeks,
                ..Default::default()
            };
            let raw = if report == "health" {
                self.item_health(item_req)?
            } else {
                self.item_standup(item_req)?
            };
            let mut parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
                ErrorData::internal_error(
                    format!(
                        "failed to parse {report} report for project {}: {e}",
                        project.name
                    ),
                    None,
                )
            })?;
            if let Some(obj) = parsed.as_object_mut() {
                obj.insert(
                    "project".to_string(),
                    serde_json::Value::String(project.name.clone()),
                );
            }
            rows.push(parsed);
        }
        Ok(
            serde_json::to_string_pretty(
                &serde_json::json!({ "report": report, "projects": rows }),
            )
            .unwrap_or_default(),
        )
    }
}
