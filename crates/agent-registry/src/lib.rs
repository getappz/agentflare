pub mod detect;
pub mod registry;
pub use detect::{
    DetectedAgent, RealVersionRunner, VersionCacheEntry, VersionRunner, detect_all,
    detect_all_with, find_binary, resolve_version, resolve_version_with,
};
pub use registry::{
    Agent, AgentSpec, REGISTRY, Tier, autonomous_args, canonicalize, headless_args, spec,
};
