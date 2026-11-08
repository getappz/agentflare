use clap::{Args, ValueEnum};
use flare_output::Llm;
use flare_summarize::summary::{self, Format, Length, SummaryOptions};
use std::path::PathBuf;

#[derive(Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    Text,
    #[value(alias = "md")]
    Markdown,
    Json,
}

impl From<OutputFormat> for Format {
    fn from(value: OutputFormat) -> Self {
        match value {
            OutputFormat::Text => Self::Text,
            OutputFormat::Markdown => Self::Markdown,
            OutputFormat::Json => Self::Json,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub enum SummaryLength {
    Short,
    Medium,
    Long,
    Xl,
    Xxl,
}

impl From<SummaryLength> for Length {
    fn from(value: SummaryLength) -> Self {
        match value {
            SummaryLength::Short => Self::Short,
            SummaryLength::Medium => Self::Medium,
            SummaryLength::Long => Self::Long,
            SummaryLength::Xl => Self::Xl,
            SummaryLength::Xxl => Self::Xxl,
        }
    }
}

#[derive(Args)]
pub struct SummarizeArgs {
    /// YouTube URL or video ID. Summarizes by default; --extract returns only captions.
    pub input: String,
    /// Caption language preference, in order; repeat for alternatives.
    #[arg(long = "language", alias = "caption-language", default_value = "en")]
    pub languages: Vec<String>,
    /// Retrieve captions without calling a model or collecting description links.
    #[arg(long, alias = "extract-only", conflicts_with_all = ["length", "summary_language", "prompt", "prompt_file", "model"])]
    pub extract: bool,
    /// Output format (default: markdown). md is an alias for markdown.
    #[arg(long, value_enum, conflicts_with = "json")]
    pub format: Option<OutputFormat>,
    /// Shortcut for --format json. Extraction keeps the original transcript JSON shape.
    #[arg(long)]
    pub json: bool,
    /// Include timestamp links/text in extracted Markdown/text; JSON always preserves times.
    #[arg(long, requires = "extract")]
    pub timestamps: bool,
    /// Approximate summary length: 100, 250, 500, 1000, or 2000 words.
    #[arg(long, value_enum, default_value = "long")]
    pub length: SummaryLength,
    /// Language for the summary prose (default: transcript language).
    #[arg(long)]
    pub summary_language: Option<String>,
    /// Additional focus/style instructions; the structured output schema is preserved.
    #[arg(long, conflicts_with = "prompt_file")]
    pub prompt: Option<String>,
    /// Read additional focus/style instructions from a UTF-8 file.
    #[arg(long, conflicts_with = "prompt")]
    pub prompt_file: Option<PathBuf>,
    /// Model override for the existing Anthropic API or authenticated Claude CLI.
    #[arg(long)]
    pub model: Option<String>,
    /// Use HTTP extraction only; never launch a browser.
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
        flare_summarize::video_id(&self.input)?;
        let prompt = if let Some(path) = self.prompt_file {
            Some(
                std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read prompt file {}: {e}", path.display()))?,
            )
        } else {
            self.prompt
        };
        let options = SummaryOptions {
            length: self.length.into(),
            language: self.summary_language,
            prompt,
        };
        options.validate()?;
        let model = self.model.or_else(|| {
            std::env::var("FLARE_SUMMARIZE_MODEL")
                .ok()
                .filter(|s| !s.trim().is_empty())
        });
        if model.as_ref().is_some_and(|s| s.trim().is_empty()) {
            return Err("model must not be empty".into());
        }
        let format = if self.json {
            Format::Json
        } else {
            self.format.unwrap_or(OutputFormat::Markdown).into()
        };
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
        if self.extract {
            println!(
                "{}",
                summary::render_transcript(&transcript, format, self.timestamps)?
            );
            return Ok(());
        }
        let description = match runtime.block_on(flare_summarize::references::fetch_description(&self.input)) {
            Ok(description) => Ok(description),
            Err(direct) if !self.fetch_only => flare_summarize::references::browser_description(&self.input, &mut runner)
                .map_err(|browser| format!("description HTTP retrieval failed: {direct}; browser retrieval failed: {browser}")),
            Err(error) => Err(format!("description HTTP retrieval failed: {error}")),
        };
        let llm = flare_output::TextLlm { model };
        let mut report = summary::summarize_with(
            &transcript,
            description.as_deref().ok(),
            &options,
            &mut |prompt| {
                llm.call(prompt).map_err(|e| {
                    format!("summary model failed: {e}; use --extract for model-free captions")
                })
            },
        )?;
        if let Err(error) = description {
            report.warnings.push(error);
        }
        println!("{}", report.render(format)?);
        Ok(())
    }
}
