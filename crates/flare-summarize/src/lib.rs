//! Fetch-first transcript extraction for the summarize workflow.

mod browser;

use serde::{Deserialize, Serialize};
use std::future::Future;
use std::time::Duration;

/// An existing agentflare browser runner, bound to an isolated session.
pub type BrowserRunner<'a> = dyn FnMut(&str, &[String]) -> Result<String, String> + 'a;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub video_id: String,
    pub url: String,
    pub language: String,
    pub source: String,
    pub generated: Option<bool>,
    pub segments: Vec<Segment>,
}

impl Transcript {
    pub fn validate(&self) -> Result<(), String> {
        if self.segments.is_empty() || self.segments.iter().all(|s| s.text.trim().is_empty()) {
            return Err("empty transcript; no captions available".into());
        }
        if self
            .segments
            .iter()
            .any(|s| !s.start.is_finite() || s.start < 0.0 || s.text.trim().is_empty())
        {
            return Err("invalid caption timestamp or empty segment".into());
        }
        if self.segments.windows(2).any(|s| s[1].start < s[0].start) {
            return Err("caption timestamps are out of order".into());
        }
        Ok(())
    }

    pub fn render(&self) -> String {
        let mut output = format!(
            "{}\nLanguage: {} | Source: {}\n\n",
            self.url, self.language, self.source
        );
        for segment in &self.segments {
            let seconds = segment.start.floor() as u64;
            output.push_str(&format!(
                "[{:02}:{:02}:{:02}] {}\n",
                seconds / 3600,
                seconds / 60 % 60,
                seconds % 60,
                segment.text
            ));
        }
        output
    }
}

/// Validate a video ID or supported YouTube URL before any external work.
pub fn video_id(input: &str) -> Result<String, String> {
    let valid = |s: &str| {
        s.len() == 11
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    };
    let input = input.trim();
    if valid(input) {
        return Ok(input.into());
    }
    let parsed =
        url::Url::parse(input).map_err(|_| "expected a YouTube URL or 11-character video ID")?;
    if !matches!(parsed.scheme(), "https" | "http")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("expected an HTTP(S) YouTube URL".into());
    }
    let id = match parsed.host_str() {
        Some("youtu.be" | "www.youtu.be") => parsed
            .path_segments()
            .and_then(|mut parts| parts.next())
            .map(str::to_owned),
        Some("youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com") => {
            if parsed.path() == "/watch" {
                parsed
                    .query_pairs()
                    .find(|(key, _)| key == "v")
                    .map(|(_, value)| value.into_owned())
            } else {
                let parts: Vec<_> = parsed.path_segments().into_iter().flatten().collect();
                match parts.as_slice() {
                    ["shorts" | "embed" | "live", id] => Some((*id).to_owned()),
                    _ => None,
                }
            }
        }
        _ => return Err("only YouTube video URLs are supported".into()),
    };
    id.filter(|s| valid(s))
        .ok_or_else(|| "missing or invalid YouTube video ID".into())
}

/// Extract captions directly, falling back to the supplied browser only on failure.
pub async fn extract(
    input: &str,
    languages: &[String],
    browser: Option<&mut BrowserRunner<'_>>,
) -> Result<Transcript, String> {
    let id = video_id(input)?;
    extract_with(&id, languages, fetch(&id, languages), browser).await
}

async fn fetch(id: &str, languages: &[String]) -> Result<Transcript, String> {
    let client = youtube_transcript_api::YouTubeTranscriptApi::builder()
        .timeout(Duration::from_secs(15))
        .match_language_variants(true)
        .build()
        .map_err(|e| e.to_string())?;
    let fetched = tokio::time::timeout(Duration::from_secs(30), client.fetch(id, languages, false))
        .await
        .map_err(|_| "direct transcript retrieval timed out after 30 seconds".to_owned())?
        .map_err(|e| e.to_string())?;
    Ok(Transcript {
        video_id: id.into(),
        url: format!("https://www.youtube.com/watch?v={id}"),
        language: fetched.language_code,
        source: "fetch".into(),
        generated: Some(fetched.is_generated),
        segments: fetched
            .snippets
            .into_iter()
            .filter(|s| !s.text.trim().is_empty())
            .map(|s| Segment {
                start: s.start,
                text: s.text.trim().into(),
            })
            .collect(),
    })
}

async fn extract_with<F>(
    id: &str,
    languages: &[String],
    fetch: F,
    browser: Option<&mut BrowserRunner<'_>>,
) -> Result<Transcript, String>
where
    F: Future<Output = Result<Transcript, String>>,
{
    if languages.is_empty() || languages.iter().any(|s| s.trim().is_empty()) {
        return Err("at least one non-empty caption language is required".into());
    }
    let direct = fetch.await.and_then(|transcript| {
        transcript.validate()?;
        Ok(transcript)
    });
    let error = match direct {
        Ok(transcript) => return Ok(transcript),
        Err(error) => error,
    };
    let Some(runner) = browser else {
        return Err(format!(
            "direct retrieval failed: {error}; browser fallback disabled"
        ));
    };
    browser::extract(id, languages, runner).map_err(|fallback| {
        format!("direct retrieval failed: {error}; browser fallback failed: {fallback}")
    })
}

#[cfg(test)]
mod tests;
