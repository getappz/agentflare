use clap::Args;

#[derive(Args)]
pub struct SummarizeArgs {
    /// YouTube URL or video ID. Outputs captions; use the summarize skill to write a summary.
    pub input: String,
    /// Caption language preference, in order; repeat for alternatives.
    #[arg(long = "language", default_value = "en")]
    pub languages: Vec<String>,
    /// Emit complete transcript metadata and timestamped segments as JSON.
    #[arg(long)]
    pub json: bool,
    /// Use direct HTTP extraction only; never launch a browser.
    #[arg(long)]
    pub fetch_only: bool,
    /// Do not auto-install the browser sidecar when fallback is needed.
    #[arg(long)]
    pub no_auto_install: bool,
}

impl SummarizeArgs {
    pub fn run(self) {
        if let Err(error) = self.execute() {
            crate::ui::error(&error);
            std::process::exit(1);
        }
    }

    fn execute(self) -> Result<(), String> {
        // Validate before constructing a runtime or invoking the browser installer.
        flare_summarize::video_id(&self.input)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let session = format!(
            "af-summarize-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        );
        let auto_install = !self.no_auto_install && flare_browser::auto_install_enabled();
        let mut runner = |action: &str, args: &[String]| {
            super::browser::exec_raw(&session, action, args, &[], auto_install)
        };
        let browser: Option<&mut flare_summarize::BrowserRunner<'_>> = if self.fetch_only {
            None
        } else {
            Some(&mut runner)
        };
        let transcript = runtime.block_on(flare_summarize::extract(
            &self.input,
            &self.languages,
            browser,
        ))?;
        if self.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&transcript).map_err(|e| e.to_string())?
            );
        } else {
            print!("{}", transcript.render());
        }
        Ok(())
    }
}
