use clap::Args;

#[derive(Args)]
pub struct InitArgs {
    #[arg(long, value_enum)]
    pub agent: agent_registry::Agent,
    #[arg(long, short = 'y')]
    pub yes: bool,
}

impl InitArgs {
    pub fn run(self) {
        crate::init::run(self.agent.as_str(), self.yes);
    }
}
