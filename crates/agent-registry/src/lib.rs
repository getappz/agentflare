pub mod detect;
pub mod registry;
pub use registry::{Agent, AgentSpec, Tier, REGISTRY, spec};
pub use detect::{detect_all, detect_all_with, find_binary, resolve_version, resolve_version_with, DetectedAgent, VersionRunner, RealVersionRunner, VersionCacheEntry};
