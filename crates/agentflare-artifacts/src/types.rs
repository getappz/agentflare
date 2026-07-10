use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactType {
    Html,
    Markdown,
    Mermaid,
    Diagram,
    Text,
}

impl fmt::Display for ArtifactType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtifactType::Html => write!(f, "html"),
            ArtifactType::Markdown => write!(f, "markdown"),
            ArtifactType::Mermaid => write!(f, "mermaid"),
            ArtifactType::Diagram => write!(f, "diagram"),
            ArtifactType::Text => write!(f, "text"),
        }
    }
}

impl ArtifactType {
    pub fn mime_type(&self) -> &str {
        match self {
            ArtifactType::Html => "text/html",
            ArtifactType::Markdown => "text/markdown",
            ArtifactType::Mermaid => "text/plain",
            ArtifactType::Diagram => "image/svg+xml",
            ArtifactType::Text => "text/plain",
        }
    }
}

impl From<&str> for ArtifactType {
    fn from(s: &str) -> Self {
        match s {
            "html" => ArtifactType::Html,
            "markdown" | "md" => ArtifactType::Markdown,
            "mermaid" => ArtifactType::Mermaid,
            "diagram" => ArtifactType::Diagram,
            _ => ArtifactType::Text,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub artifact_type: ArtifactType,
    pub content: String,
    pub session_id: String,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtifactSummary {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub artifact_type: ArtifactType,
    pub session_id: String,
    pub created_at: u64,
    pub updated_at: u64,
}

impl From<&Artifact> for ArtifactSummary {
    fn from(a: &Artifact) -> Self {
        ArtifactSummary {
            id: a.id.clone(),
            name: a.name.clone(),
            artifact_type: a.artifact_type.clone(),
            session_id: a.session_id.clone(),
            created_at: a.created_at,
            updated_at: a.updated_at,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishRequest {
    pub name: String,
    #[serde(rename = "type")]
    pub artifact_type: ArtifactType,
    pub content: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishResponse {
    pub id: String,
    pub url: String,
    pub session_id: String,
}
