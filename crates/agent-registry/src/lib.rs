pub mod detect;
pub mod registry;
pub mod router;
pub use detect::{
    DetectedAgent, RealVersionRunner, VersionCacheEntry, VersionRunner, detect_all,
    detect_all_with, find_binary, resolve_version, resolve_version_with,
};
pub use registry::{
    Agent, AgentSpec, REGISTRY, Tier, agent_by_name, autonomous_args, canonicalize, headless_args,
    spec,
};
pub use router::{
    RouteDecision, RouterConfig, RouterRule, RuleMatch, TaskContext, parse_router_config, route,
};
