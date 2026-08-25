use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
struct RawAppManifest {
    name: String,
    version: String,
    workflow: String,
    #[serde(default)]
    sandbox_profile: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AppManifest {
    pub name: String,
    pub version: String,
    pub workflow: PathBuf,
    pub sandbox_profile: Option<String>,
}

pub fn load_app_manifest(app_dir: &Path) -> Result<AppManifest, String> {
    let path = app_dir.join("app.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let raw: RawAppManifest =
        toml::from_str(&text).map_err(|e| format!("{}: invalid app.toml: {e}", path.display()))?;
    Ok(AppManifest {
        name: raw.name,
        version: raw.version,
        workflow: app_dir.join(raw.workflow),
        sandbox_profile: raw.sandbox_profile,
    })
}

#[derive(Debug, Deserialize)]
pub struct ToolsManifest {
    #[serde(default)]
    pub servers: HashMap<String, gateway_registry::ServerConfig>,
}

pub fn load_tools_manifest(app_dir: &Path) -> Result<Option<ToolsManifest>, String> {
    let path = app_dir.join("tools.toml");
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let parsed: ToolsManifest =
        toml::from_str(&text).map_err(|e| format!("{}: invalid tools.toml: {e}", path.display()))?;
    Ok(Some(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn loads_a_minimal_app_toml() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join("app.toml")).unwrap();
        writeln!(
            f,
            r#"
            name = "auto-company"
            version = "0.1.0"
            workflow = "workflow.json"
            "#
        )
        .unwrap();

        let manifest = load_app_manifest(dir.path()).unwrap();
        assert_eq!(manifest.name, "auto-company");
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.workflow, dir.path().join("workflow.json"));
        assert_eq!(manifest.sandbox_profile, None);
    }

    #[test]
    fn missing_app_toml_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_app_manifest(dir.path()).unwrap_err();
        assert!(err.contains("app.toml"), "error should name the missing file: {err}");
    }
}
