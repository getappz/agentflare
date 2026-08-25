use crate::manifest::ToolsManifest;
use gateway_registry::ServerConfig;
use std::path::Path;

pub fn project_for_claude_code(
    app_dir: &Path,
    scratch_dir: &Path,
    tools: Option<&ToolsManifest>,
) -> Result<(), String> {
    project_dir(
        &app_dir.join("personas"),
        &scratch_dir.join(".claude/agents"),
        "md",
    )?;
    project_skills(&app_dir.join("skills"), &scratch_dir.join(".claude/skills"))?;

    let settings_dir = scratch_dir.join(".claude");
    std::fs::create_dir_all(&settings_dir).map_err(|e| e.to_string())?;
    std::fs::write(
        settings_dir.join("settings.json"),
        serde_json::to_vec_pretty(&serde_json::json!({ "enableAllProjectMcpServers": true }))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    if let Some(tools) = tools {
        write_mcp_json(scratch_dir, tools)?;
    }
    Ok(())
}

fn project_dir(src: &Path, dst: &Path, ext: &str) -> Result<(), String> {
    if !src.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.path().extension().is_some_and(|e| e == ext) {
            let target = dst.join(entry.file_name());
            std::fs::copy(entry.path(), target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn project_skills(src: &Path, dst: &Path) -> Result<(), String> {
    if !src.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "md") {
            let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
            let skill_dir = dst.join(&stem);
            std::fs::create_dir_all(&skill_dir).map_err(|e| e.to_string())?;
            std::fs::copy(&path, skill_dir.join("SKILL.md")).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn write_mcp_json(scratch_dir: &Path, tools: &ToolsManifest) -> Result<(), String> {
    let mut servers = serde_json::Map::new();
    for (name, cfg) in &tools.servers {
        let entry = match cfg {
            ServerConfig::McpStdio { command, args, .. } => serde_json::json!({
                "command": command,
                "args": args,
            }),
            ServerConfig::McpHttp { url, .. } => serde_json::json!({ "url": url }),
        };
        servers.insert(name.clone(), entry);
    }
    let doc = serde_json::json!({ "mcpServers": serde_json::Value::Object(servers) });
    std::fs::write(
        scratch_dir.join(".mcp.json"),
        serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_personas_and_skills_into_claude_layout() {
        let app_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(app_dir.path().join("personas")).unwrap();
        std::fs::write(
            app_dir.path().join("personas/ceo.md"),
            "# CEO\nYou lead the company.",
        )
        .unwrap();
        std::fs::create_dir_all(app_dir.path().join("skills")).unwrap();
        std::fs::write(app_dir.path().join("skills/pricing.md"), "# Pricing skill").unwrap();

        let scratch = tempfile::tempdir().unwrap();
        project_for_claude_code(app_dir.path(), scratch.path(), None).unwrap();

        let persona =
            std::fs::read_to_string(scratch.path().join(".claude/agents/ceo.md")).unwrap();
        assert_eq!(persona, "# CEO\nYou lead the company.");

        let skill = std::fs::read_to_string(scratch.path().join(".claude/skills/pricing/SKILL.md"))
            .unwrap();
        assert_eq!(skill, "# Pricing skill");

        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(scratch.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            settings["enableAllProjectMcpServers"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn writes_mcp_json_when_tools_manifest_present() {
        let app_dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let mut servers = std::collections::HashMap::new();
        servers.insert(
            "pricing-api".to_string(),
            gateway_registry::ServerConfig::McpStdio {
                command: "pricing-mcp".to_string(),
                args: vec![],
                auth_ref: None,
                auth_env: None,
            },
        );
        let tools = ToolsManifest { servers };

        project_for_claude_code(app_dir.path(), scratch.path(), Some(&tools)).unwrap();

        let mcp_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(scratch.path().join(".mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            mcp_json["mcpServers"]["pricing-api"]["command"],
            "pricing-mcp"
        );
    }
}
