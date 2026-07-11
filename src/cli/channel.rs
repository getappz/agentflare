use clap::{Args, Subcommand};

/// Send messages out to chat platforms (Telegram / Slack / Discord). Bot tokens
/// are read from the encrypted gateway secret store.
#[derive(Args)]
pub struct ChannelArgs {
    #[command(subcommand)]
    pub action: ChannelAction,
}

#[derive(Subcommand)]
pub enum ChannelAction {
    /// Send a text message to a chat platform.
    Send {
        /// Platform: telegram | slack | discord.
        #[arg(long)]
        to: String,
        /// Recipient id (Telegram chat_id, Slack/Discord channel id).
        #[arg(long)]
        target: String,
        /// The message text.
        message: String,
    },
}

impl ChannelArgs {
    pub fn run(self) {
        match self.action {
            ChannelAction::Send {
                to,
                target,
                message,
            } => {
                let Some(platform) = crate::channels::Platform::parse(&to) else {
                    eprintln!(
                        "error: unknown platform '{to}' (expected telegram, slack, or discord)"
                    );
                    std::process::exit(1);
                };
                let conn = match crate::db::open() {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("channel: cannot open database: {e}");
                        std::process::exit(1);
                    }
                };
                match crate::channels::send_message(&conn, platform, &target, &message) {
                    Ok(()) => println!("sent to {to}:{target}"),
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}
