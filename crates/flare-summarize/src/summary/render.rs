use super::{Format, SummaryReport};
use crate::Transcript;

fn time(start: f64) -> String {
    let seconds = start.floor() as u64;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn plain_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn markdown_text(text: &str) -> String {
    plain_line(text)
        .replace('\\', "\\\\")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn link(url: &str) -> String {
    url.replace('(', "%28")
        .replace(')', "%29")
        .replace('<', "%3C")
        .replace('>', "%3E")
        .replace('"', "%22")
}

pub fn render_transcript(
    transcript: &Transcript,
    format: Format,
    timestamps: bool,
) -> Result<String, String> {
    transcript.validate()?;
    if format == Format::Json {
        return serde_json::to_string_pretty(transcript).map_err(|e| e.to_string());
    }
    let mut output = format!(
        "{}\nLanguage: {} | Source: {}\n\n",
        transcript.url, transcript.language, transcript.source
    );
    for segment in &transcript.segments {
        let text = if format == Format::Markdown {
            markdown_text(&segment.text)
        } else {
            plain_line(&segment.text)
        };
        let prefix = if !timestamps {
            String::new()
        } else if format == Format::Markdown {
            format!(
                "[{}]({}&t={}s) ",
                time(segment.start),
                transcript.url,
                segment.start.floor() as u64
            )
        } else {
            format!("[{}] ", time(segment.start))
        };
        output.push_str(&format!("{prefix}{text}\n"));
    }
    Ok(output)
}

impl SummaryReport {
    pub fn render(&self, format: Format) -> Result<String, String> {
        if format == Format::Json {
            return serde_json::to_string_pretty(self).map_err(|e| e.to_string());
        }
        let md = format == Format::Markdown;
        let text = |s: &str| if md { markdown_text(s) } else { plain_line(s) };
        let mut output = format!(
            "{}\n\n{}\n\n",
            self.transcript.url,
            text(&self.summary.overview)
        );
        for point in &self.summary.takeaways {
            let stamp = time(point.start);
            let url = format!("{}&t={}s", self.transcript.url, point.start.floor() as u64);
            let prefix = if md {
                format!("[{stamp}]({url})")
            } else {
                format!("[{stamp}] ({url})")
            };
            output.push_str(&format!("- {prefix}: {}\n", text(&point.text)));
        }
        output.push_str(if md {
            "\n## References and resources\n\n"
        } else {
            "\nReferences and resources\n\n"
        });
        if self.references.is_empty() {
            output.push_str("No reference URLs were supplied in the available sources.\n");
        }
        for reference in &self.references {
            let label = text(&reference.label);
            let resource = if md {
                format!("[{label}]({})", link(&reference.url))
            } else {
                format!("{label}: {}", reference.url)
            };
            let promotion = if reference.promotional {
                "; promotional/referral link"
            } else {
                ""
            };
            output.push_str(&format!(
                "- {resource} ({}{promotion})\n",
                text(&reference.source)
            ));
        }
        let has_repo = self.references.iter().any(|r| {
            url::Url::parse(&r.url).ok().is_some_and(|u| {
                matches!(
                    u.host_str(),
                    Some("github.com" | "gitlab.com" | "codeberg.org" | "bitbucket.org")
                ) && u
                    .path_segments()
                    .into_iter()
                    .flatten()
                    .filter(|s| !s.is_empty())
                    .count()
                    >= 2
            })
        });
        if !has_repo {
            output.push_str("No repository URL was supplied in the sources checked.\n");
        }
        for warning in &self.warnings {
            output.push_str(&format!("\nNote: {}\n", text(warning)));
        }
        Ok(output)
    }
}
