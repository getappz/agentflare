use clap::Args;

/// Diagnose agent config wiring and report problems.
#[derive(Args)]
pub struct DoctorArgs {
    /// Check only this agent's config wiring instead of every agent detected on PATH.
    #[arg(long, value_enum)]
    pub agent: Option<agent_registry::Agent>,
    #[arg(long)]
    pub json: bool,
}

impl DoctorArgs {
    pub fn run(self) {
        crate::doctor::run(self.agent.map(agent_registry::Agent::as_str), self.json);
    }
}
