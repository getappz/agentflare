//! Harness² method adapted from google-research/harness2 (Google LLC, Apache-2.0).
//! Runtime effects belong to the injected backend; orchestration uses this crate's engine.
mod phases;
mod prompts;
mod tree;
mod types;
pub use phases::build_refinement_workflow;
pub use tree::validate_tree;
pub use types::*;

pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
pub fn harness_identity(tree: &HarnessTree) -> String {
    digest(&serde_json::to_vec(tree).expect("string map serializes"))
}
