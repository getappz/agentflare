pub mod confirm;
pub mod workspace_graph;

use std::path::{Path, PathBuf};
use workspace_graph::{WorkspaceGraph, WorkspaceGraphError};

#[derive(Debug)]
pub enum ImpactReason {
    OwnIntegrationTest,
    CrossCrateReference { via_crate: String },
}

#[derive(Debug)]
pub struct ImpactHit {
    pub file: PathBuf,
    pub line: usize,
    pub text: String,
    pub reason: ImpactReason,
}

#[derive(Debug)]
pub struct ImpactReport {
    pub target: PathBuf,
    pub owner_crate: String,
    pub hits: Vec<ImpactHit>,
}

#[derive(Debug)]
pub enum ImpactError {
    Graph(WorkspaceGraphError),
    NotInWorkspace(PathBuf),
}

impl std::fmt::Display for ImpactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Graph(e) => write!(f, "{e}"),
            Self::NotInWorkspace(p) => {
                write!(f, "{} is not part of this workspace", p.display())
            }
        }
    }
}

pub fn impact_for_path(repo_root: &Path, target: &Path) -> Result<ImpactReport, ImpactError> {
    let graph = WorkspaceGraph::load(repo_root).map_err(ImpactError::Graph)?;
    let absolute_target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        repo_root.join(target)
    };
    let owner = graph
        .resolve_owner_crate(&absolute_target)
        .ok_or_else(|| ImpactError::NotInWorkspace(absolute_target.clone()))?;

    let mut hits = Vec::new();

    // The owning crate's own tests/benches/examples always depend on it —
    // this is the exact case lean-ctx's basename-only resolver missed, and
    // Cargo's own metadata already proves it with zero ambiguity.
    for test_path in &owner.own_test_paths {
        hits.push(ImpactHit {
            file: test_path.clone(),
            line: 1,
            text: format!("integration test of crate `{}`", owner.name),
            reason: ImpactReason::OwnIntegrationTest,
        });
    }

    // Cross-crate: cargo metadata tells us which crates COULD depend on the
    // owner (coarse, crate-level); confirm with an actual reference search
    // scoped to each candidate, so an unused/stale dependency in Cargo.toml
    // doesn't produce a false positive.
    let crate_ident = owner.name.replace('-', "_");
    for dependent_name in graph.reverse_dependents(&owner.name) {
        let Some(dependent) = graph.crate_by_name(&dependent_name) else {
            continue;
        };
        for hit in confirm::search_crate_dir(&dependent.manifest_dir, &crate_ident) {
            hits.push(ImpactHit {
                file: hit.file,
                line: hit.line,
                text: hit.text,
                reason: ImpactReason::CrossCrateReference {
                    via_crate: dependent_name.clone(),
                },
            });
        }
    }

    Ok(ImpactReport {
        target: absolute_target,
        owner_crate: owner.name.clone(),
        hits,
    })
}
