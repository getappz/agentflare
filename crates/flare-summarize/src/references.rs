//! Source-backed resource links; discovery never downloads linked resources.
use crate::Transcript;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    pub label: String,
    pub url: String,
    pub source: String,
    pub promotional: bool,
}

/// Decode the embedded player object, including JSON escapes in its description.
pub fn description_from_html(html: &str) -> Option<String> {
    for (offset, marker) in html.match_indices("ytInitialPlayerResponse") {
        let tail = html[offset + marker.len()..].trim_start();
        let Some(tail) = tail.strip_prefix('=') else {
            continue;
        };
        let Some(Ok(value)) = serde_json::Deserializer::from_str(tail.trim_start())
            .into_iter::<serde_json::Value>()
            .next()
        else {
            continue;
        };
        if let Some(description) = value
            .pointer("/videoDetails/shortDescription")
            .and_then(|v| v.as_str())
        {
            return Some(description.to_owned());
        }
    }
    None
}

pub async fn fetch_description(input: &str) -> Result<String, String> {
    use youtube_transcript_api::http::{
        HeaderMap, HeaderValue, HttpClient, HttpRequest, Method, ReqwestClient,
    };
    let id = crate::video_id(input)?;
    let client = ReqwestClient::with_options(None, Some(Duration::from_secs(15)))
        .map_err(|e| e.to_string())?;
    let mut headers = HeaderMap::new();
    headers.insert("user-agent", HeaderValue::from_static("Mozilla/5.0"));
    let response = client
        .send(HttpRequest::new(
            Method::GET,
            format!("https://www.youtube.com/watch?v={id}"),
            headers,
            None,
        ))
        .await
        .map_err(|e| e.to_string())?;
    if !response.status.is_success() {
        return Err(format!("description request returned {}", response.status));
    }
    description_from_html(&response.body)
        .ok_or_else(|| "video description was not present in the watch page".into())
}

pub fn browser_description(
    input: &str,
    runner: &mut crate::BrowserRunner<'_>,
) -> Result<String, String> {
    let id = crate::video_id(input)?;
    let result = (|| {
        runner(
            "open",
            &[format!("https://www.youtube.com/watch?v={id}&hl=en")],
        )?;
        let output = runner("eval", &[r#"(async () => {
          for (let i = 0; i < 40; i++) {
            const p = window.ytInitialPlayerResponse || document.querySelector('#movie_player')?.getPlayerResponse?.();
            if (typeof p?.videoDetails?.shortDescription === 'string') return JSON.stringify({description: p.videoDetails.shortDescription});
            await new Promise(resolve => setTimeout(resolve, 250));
          }
          throw new Error('Video description unavailable');
        })()"#.into()])?;
        let mut data: serde_json::Value =
            serde_json::from_str(&output).map_err(|e| e.to_string())?;
        if let Some(inner) = data.as_str() {
            data = serde_json::from_str(inner).map_err(|e| e.to_string())?;
        }
        data["description"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "browser description unavailable".into())
    })();
    let _ = runner("close", &[]);
    result
}

pub fn collect_references(description: Option<&str>, transcript: &Transcript) -> Vec<Reference> {
    let mut seen = HashSet::new();
    let mut references = Vec::new();
    let mut add = |line: &str, source: &str| {
        for token in line.split_whitespace() {
            let Some(start) = token.find("https://").or_else(|| token.find("http://")) else {
                continue;
            };
            let mut candidate =
                token[start..].trim_end_matches(['.', ',', ';', '!', '"', '\'', '>']);
            while candidate.ends_with(')')
                && candidate.matches(')').count() > candidate.matches('(').count()
            {
                candidate = &candidate[..candidate.len() - 1];
            }
            if let Some(end) = candidate.find("](") {
                candidate = &candidate[..end];
            }
            let Ok(url) = url::Url::parse(candidate) else {
                continue;
            };
            if !matches!(url.scheme(), "https" | "http")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                continue;
            }
            if !seen.insert(candidate.to_owned()) {
                continue;
            }
            let label = line
                .split(candidate)
                .next()
                .unwrap_or("")
                .trim()
                .trim_end_matches(':')
                .trim();
            let lower = line.to_lowercase();
            references.push(Reference {
                label: if label.is_empty() {
                    url.host_str().unwrap_or("Resource").into()
                } else {
                    label.to_owned()
                },
                url: candidate.to_owned(),
                source: source.into(),
                promotional: ["affiliate", "referral", "discount", "best deal"]
                    .iter()
                    .any(|word| lower.contains(word)),
            });
        }
    };
    for line in description.unwrap_or("").lines() {
        add(line, "description");
    }
    for segment in &transcript.segments {
        add(
            &segment.text,
            &format!("transcript at {:.3}s", segment.start),
        );
    }
    references
}
