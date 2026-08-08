//! GitHub-as-coordination-substrate bridge. See the design spec: issues
//! labelled `agentflare` form an open pull queue that any instance may claim.

pub mod claim;
pub mod config;
pub mod items;
pub mod marker;
pub mod queue_status;
pub mod runner;
pub mod tick;

#[cfg(test)]
mod tests {
    mod live_github;
    mod two_instance;
}
