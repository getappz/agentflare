use clap::Args;

#[derive(Args)]
pub struct UninstallArgs {
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub keep_config: bool,
    #[arg(long)]
    pub keep_binary: bool,
}

impl UninstallArgs {
    pub fn run(self) {
        crate::uninstall::run(self.dry_run, self.keep_config, self.keep_binary);
    }
}
