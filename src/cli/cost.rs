use clap::Args;

#[derive(Args)]
pub struct CostArgs {
    #[arg(long)]
    pub days: Option<u32>,
    #[arg(long)]
    pub by_project: bool,
}

impl CostArgs {
    pub fn run(self) {
        crate::cost::run(self.days, self.by_project);
    }
}
