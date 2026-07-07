use clap::Args;

#[derive(Args)]
pub struct UpdateArgs {
    pub version: Option<String>,
    #[arg(long)]
    pub check: bool,
    #[arg(long)]
    pub quiet: bool,
}

impl UpdateArgs {
    pub fn run(self) {
        crate::update::run(self.version, self.check, self.quiet);
    }
}
