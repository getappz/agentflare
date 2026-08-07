use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
pub enum WorkspaceGraphError {
    NoCargoToml,
    CargoMetadataFailed(String),
}

impl std::fmt::Display for WorkspaceGraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCargoToml => write!(
                f,
                "no Cargo.toml found — `agentflare code impact` supports Rust workspaces only, for now"
            ),
            Self::CargoMetadataFailed(msg) => write!(f, "cargo metadata failed: {msg}"),
        }
    }
}

pub struct CrateInfo {
    pub name: String,
    pub manifest_dir: PathBuf,
    pub own_test_paths: Vec<PathBuf>,
}

pub struct WorkspaceGraph {
    crates: Vec<CrateInfo>,
    // crate name -> set of crate names that directly depend on it
    dependents: HashMap<String, HashSet<String>>,
}

#[derive(serde::Deserialize)]
struct Metadata {
    packages: Vec<MetaPackage>,
    workspace_members: Vec<String>,
    resolve: MetaResolve,
}

#[derive(serde::Deserialize)]
struct MetaPackage {
    id: String,
    name: String,
    manifest_path: String,
    targets: Vec<MetaTarget>,
}

#[derive(serde::Deserialize)]
struct MetaTarget {
    kind: Vec<String>,
    src_path: String,
}

#[derive(serde::Deserialize)]
struct MetaResolve {
    nodes: Vec<MetaNode>,
}

#[derive(serde::Deserialize)]
struct MetaNode {
    id: String,
    deps: Vec<MetaDep>,
}

#[derive(serde::Deserialize)]
struct MetaDep {
    pkg: String,
}

impl WorkspaceGraph {
    pub fn load(repo_root: &Path) -> Result<Self, WorkspaceGraphError> {
        if !repo_root.join("Cargo.toml").exists() {
            return Err(WorkspaceGraphError::NoCargoToml);
        }
        let output = Command::new("cargo")
            .args(["metadata", "--format-version", "1"])
            .current_dir(repo_root)
            .output()
            .map_err(|e| WorkspaceGraphError::CargoMetadataFailed(e.to_string()))?;
        if !output.status.success() {
            return Err(WorkspaceGraphError::CargoMetadataFailed(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        let json = String::from_utf8_lossy(&output.stdout);
        Self::from_metadata_json(&json)
            .map_err(|e| WorkspaceGraphError::CargoMetadataFailed(e.to_string()))
    }

    fn from_metadata_json(json: &str) -> Result<Self, serde_json::Error> {
        let meta: Metadata = serde_json::from_str(json)?;
        let member_ids: HashSet<&str> = meta.workspace_members.iter().map(String::as_str).collect();

        let mut id_to_name: HashMap<&str, &str> = HashMap::new();
        let mut crates = Vec::new();
        for pkg in &meta.packages {
            if !member_ids.contains(pkg.id.as_str()) {
                continue;
            }
            id_to_name.insert(&pkg.id, &pkg.name);
            let manifest_dir = Path::new(&pkg.manifest_path)
                .parent()
                .unwrap_or_else(|| Path::new(&pkg.manifest_path))
                .to_path_buf();
            let own_test_paths = pkg
                .targets
                .iter()
                .filter(|t| {
                    t.kind
                        .iter()
                        .any(|k| k == "test" || k == "bench" || k == "example")
                })
                .map(|t| PathBuf::from(&t.src_path))
                .collect();
            crates.push(CrateInfo {
                name: pkg.name.clone(),
                manifest_dir,
                own_test_paths,
            });
        }

        let mut dependents: HashMap<String, HashSet<String>> = HashMap::new();
        for node in &meta.resolve.nodes {
            if !member_ids.contains(node.id.as_str()) {
                continue;
            }
            let Some(&dependent_name) = id_to_name.get(node.id.as_str()) else {
                continue;
            };
            for dep in &node.deps {
                if !member_ids.contains(dep.pkg.as_str()) {
                    continue;
                }
                let Some(&dep_name) = id_to_name.get(dep.pkg.as_str()) else {
                    continue;
                };
                dependents
                    .entry(dep_name.to_string())
                    .or_default()
                    .insert(dependent_name.to_string());
            }
        }

        Ok(WorkspaceGraph { crates, dependents })
    }

    pub fn crate_by_name(&self, name: &str) -> Option<&CrateInfo> {
        self.crates.iter().find(|c| c.name == name)
    }

    /// Longest-matching `manifest_dir` prefix wins, so a nested crate inside
    /// another crate's directory (unusual, but not forbidden by Cargo)
    /// resolves to the more specific owner.
    pub fn resolve_owner_crate(&self, path: &Path) -> Option<&CrateInfo> {
        self.crates
            .iter()
            .filter(|c| path.starts_with(&c.manifest_dir))
            .max_by_key(|c| c.manifest_dir.as_os_str().len())
    }

    /// Transitive reverse-dependency walk. Does not include `crate_name`
    /// itself even if there's a (theoretical) cycle.
    pub fn reverse_dependents(&self, crate_name: &str) -> Vec<String> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        queue.push_back(crate_name.to_string());
        let mut result = Vec::new();
        while let Some(current) = queue.pop_front() {
            let Some(direct) = self.dependents.get(&current) else {
                continue;
            };
            for name in direct {
                if seen.insert(name.clone()) {
                    result.push(name.clone());
                    queue.push_back(name.clone());
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // A synthetic 3-crate workspace: `core` (has its own integration test
    // `core_test`), `consumer` (depends on `core` normally), and
    // `dev-consumer` (depends on `core` only as a dev-dependency) — mirrors
    // the exact shape of the bug this feature fixes: a crate's own
    // integration tests, plus another crate's dev-only dependency, both need
    // to show up as dependents.
    const FIXTURE_METADATA: &str = r#"{
        "packages": [
            {
                "id": "path+file:///ws/crates/core#0.1.0",
                "name": "core",
                "manifest_path": "/ws/crates/core/Cargo.toml",
                "targets": [
                    {"kind": ["lib"], "src_path": "/ws/crates/core/src/lib.rs"},
                    {"kind": ["test"], "src_path": "/ws/crates/core/tests/core_test.rs"}
                ]
            },
            {
                "id": "path+file:///ws/crates/consumer#0.1.0",
                "name": "consumer",
                "manifest_path": "/ws/crates/consumer/Cargo.toml",
                "targets": [
                    {"kind": ["lib"], "src_path": "/ws/crates/consumer/src/lib.rs"}
                ]
            },
            {
                "id": "path+file:///ws/crates/dev-consumer#0.1.0",
                "name": "dev-consumer",
                "manifest_path": "/ws/crates/dev-consumer/Cargo.toml",
                "targets": [
                    {"kind": ["lib"], "src_path": "/ws/crates/dev-consumer/src/lib.rs"}
                ]
            },
            {
                "id": "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0",
                "name": "serde",
                "manifest_path": "/registry/serde/Cargo.toml",
                "targets": [{"kind": ["lib"], "src_path": "/registry/serde/src/lib.rs"}]
            }
        ],
        "workspace_members": [
            "path+file:///ws/crates/core#0.1.0",
            "path+file:///ws/crates/consumer#0.1.0",
            "path+file:///ws/crates/dev-consumer#0.1.0"
        ],
        "resolve": {
            "nodes": [
                {
                    "id": "path+file:///ws/crates/core#0.1.0",
                    "deps": []
                },
                {
                    "id": "path+file:///ws/crates/consumer#0.1.0",
                    "deps": [
                        {"name": "core", "pkg": "path+file:///ws/crates/core#0.1.0", "dep_kinds": [{"kind": null}]},
                        {"name": "serde", "pkg": "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0", "dep_kinds": [{"kind": null}]}
                    ]
                },
                {
                    "id": "path+file:///ws/crates/dev-consumer#0.1.0",
                    "deps": [
                        {"name": "core", "pkg": "path+file:///ws/crates/core#0.1.0", "dep_kinds": [{"kind": "dev"}]}
                    ]
                }
            ],
            "root": "path+file:///ws/crates/consumer#0.1.0"
        }
    }"#;

    #[test]
    fn parses_crates_and_marks_own_test_targets() {
        let graph = WorkspaceGraph::from_metadata_json(FIXTURE_METADATA).unwrap();
        let core = graph.crate_by_name("core").unwrap();
        assert_eq!(core.manifest_dir, Path::new("/ws/crates/core"));
        assert_eq!(
            core.own_test_paths,
            vec![Path::new("/ws/crates/core/tests/core_test.rs")]
        );
        // external (crates.io) packages must never appear as workspace crates
        assert!(graph.crate_by_name("serde").is_none());
    }

    #[test]
    fn reverse_dependents_includes_normal_and_dev_dependencies() {
        let graph = WorkspaceGraph::from_metadata_json(FIXTURE_METADATA).unwrap();
        let mut dependents = graph.reverse_dependents("core");
        dependents.sort();
        assert_eq!(dependents, vec!["consumer", "dev-consumer"]);
    }

    #[test]
    fn resolve_owner_crate_picks_the_longest_matching_manifest_dir() {
        let graph = WorkspaceGraph::from_metadata_json(FIXTURE_METADATA).unwrap();
        let owner = graph
            .resolve_owner_crate(Path::new("/ws/crates/core/src/lib.rs"))
            .unwrap();
        assert_eq!(owner.name, "core");
        assert!(
            graph
                .resolve_owner_crate(Path::new("/ws/not-in-workspace/x.rs"))
                .is_none()
        );
    }
}
