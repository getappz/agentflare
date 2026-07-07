use clap::Args;

#[derive(Args)]
pub struct AliasArgs {
    pub preferred: Option<String>,
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub print: bool,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub shell: Option<String>,
    #[arg(long)]
    pub profile: Option<String>,
    #[arg(long)]
    pub json: bool,
}

impl AliasArgs {
    pub fn run(self) {
        crate::alias::run(
            self.preferred,
            self.force,
            self.print,
            self.yes,
            self.shell,
            self.profile,
            self.json,
        );
    }
}
