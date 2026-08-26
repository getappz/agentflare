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
    let workflow = workflow_path_within_app_dir(app_dir, &raw.workflow)?;
    Ok(AppManifest {
        name: raw.name,
        version: raw.version,
        workflow,
        sandbox_profile: raw.sandbox_profile,
    })
}

/// Rejects an absolute path or a `..` component so `app.toml`'s `workflow`
/// field can't point outside `app_dir` — apps are untrusted content (Task 7
/// treats them as importable/shareable units), so a malicious `app.toml`
/// must not be able to name an arbitrary file on disk.
fn workflow_path_within_app_dir(app_dir: &Path, workflow: &str) -> Result<PathBuf, String> {
    let rel = Path::new(workflow);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    {
        return Err(format!(
            "app.toml: workflow path '{workflow}' must be relative and stay within the app directory"
        ));
    }
    Ok(app_dir.join(rel))
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolsManifest {
    #[serde(default)]
    pub servers: HashMap<String, gateway_registry::ServerConfig>,
}

pub fn load_tools_manifest(app_dir: &Path) -> Result<Option<ToolsManifest>, String> {
    let path = app_dir.join("tools.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let parsed: ToolsManifest = toml::from_str(&text)
        .map_err(|e| format!("{}: invalid tools.toml: {e}", path.display()))?;
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
        assert!(
            err.contains("app.toml"),
            "error should name the missing file: {err}"
        );
    }

    fn write_app_toml_with_workflow(dir: &std::path::Path, workflow: &str) {
        let mut f = std::fs::File::create(dir.join("app.toml")).unwrap();
        writeln!(
            f,
            r#"
            name = "auto-company"
            version = "0.1.0"
            workflow = "{workflow}"
            "#
        )
        .unwrap();
    }

    #[test]
    fn rejects_an_absolute_workflow_path() {
        let dir = tempfile::tempdir().unwrap();
        let absolute = if cfg!(windows) {
            "C:\\\\Windows\\\\System32\\\\config"
        } else {
            "/etc/passwd"
        };
        write_app_toml_with_workflow(dir.path(), absolute);
        let err = load_app_manifest(dir.path()).unwrap_err();
        assert!(
            err.contains("must be relative"),
            "absolute workflow path should be rejected: {err}"
        );
    }

    #[test]
    fn rejects_a_parent_traversal_workflow_path() {
        let dir = tempfile::tempdir().unwrap();
        write_app_toml_with_workflow(dir.path(), "../../etc/passwd");
        let err = load_app_manifest(dir.path()).unwrap_err();
        assert!(
            err.contains("must be relative"),
            "parent-traversal workflow path should be rejected: {err}"
        );
    }
}
