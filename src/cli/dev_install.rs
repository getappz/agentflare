use clap::Args;

/// Build the current source tree and install it over the running binary.
///
/// Intended to be run from your *installed* `agentflare` inside a checkout:
/// it builds the checkout, verifies the binary, then swaps it into place using
/// the same MCP-safe replacement as `agentflare update`.
#[derive(Args)]
pub struct DevInstallArgs {
    /// Build in debug mode instead of the default `--release`.
    #[arg(long)]
    pub debug: bool,
    /// Build and verify, but report what would be installed without replacing.
    #[arg(long)]
    pub dry_run: bool,
}

impl DevInstallArgs {
    pub fn run(self) {
        crate::dev_install::run(!self.debug, self.dry_run);
    }
}
