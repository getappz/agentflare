pub mod detect;
pub mod failover;
pub mod headless;
pub mod registry;
pub mod role;
pub mod router;
pub use detect::{
    DetectedAgent, RealVersionRunner, VersionCacheEntry, VersionRunner, detect_all,
    detect_all_with, detect_present, find_binary, resolve_version, resolve_version_with,
};
pub use failover::{FailoverConfig, failover_candidates, parse_failover_config};
pub use headless::{
    HeadlessCli, ReplyFormat, Structured, autonomous_args, headless_args, headless_cli,
    pins_output_format, resume_args, structured_output,
};
pub use registry::{
    Agent, AgentSpec, REGISTRY, Tier, agent_by_name, canonicalize, clinepass_model_for_claude,
    mode_flag, spec,
};
pub use role::{
    CompiledRole, Effort, PermissionMode, RoleSpec, compile_role, prompt_with_system_fallback,
};
pub use router::{
    RouteDecision, RouterConfig, RouterRule, RuleMatch, TaskContext, model_for_task,
    parse_router_config, route,
};
