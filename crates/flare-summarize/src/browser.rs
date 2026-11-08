use crate::{BrowserRunner, Segment, Transcript};
use serde::Deserialize;

#[derive(Deserialize)]
struct Prepared {
    count: usize,
    language: String,
    generated: Option<bool>,
}

#[derive(Deserialize)]
struct Chunk {
    offset: usize,
    next: usize,
    segments: Vec<Segment>,
}

fn decode<T: serde::de::DeserializeOwned>(output: &str) -> Result<T, String> {
    let value: serde_json::Value =
        serde_json::from_str(output).map_err(|e| format!("invalid browser JSON: {e}"))?;
    if let Some(inner) = value.as_str() {
        serde_json::from_str(inner).map_err(|e| format!("invalid browser result: {e}"))
    } else {
        serde_json::from_value(value).map_err(|e| format!("invalid browser result: {e}"))
    }
}

pub(super) fn extract(
    id: &str,
    languages: &[String],
    runner: &mut BrowserRunner<'_>,
) -> Result<Transcript, String> {
    let result = read(id, languages, runner);
    // The caller gives us a dedicated session, so cleanup never closes their active tab.
    let cleanup = runner("close", &[]);
    match (result, cleanup) {
        (Ok(transcript), Ok(_)) => Ok(transcript),
        (Ok(_), Err(error)) => Err(format!(
            "transcript retrieved but browser cleanup failed: {error}"
        )),
        (Err(error), _) => Err(error),
    }
}

fn read(
    id: &str,
    languages: &[String],
    runner: &mut BrowserRunner<'_>,
) -> Result<Transcript, String> {
    let url = format!("https://www.youtube.com/watch?v={id}");
    runner("open", &[format!("{url}&hl=en")])?;
    let script = include_str!("browser_prepare.js").replace(
        "__LANGUAGES__",
        &serde_json::to_string(languages).map_err(|e| e.to_string())?,
    );
    let prepared: Prepared = decode(&runner("eval", &[script])?)?;
    if prepared.count == 0 || prepared.count > 100_000 {
        return Err("empty or excessively large browser transcript".into());
    }
    if !crate::matches_language(&prepared.language, languages) {
        return Err("browser returned captions in an unrequested language".into());
    }
    let mut segments = Vec::with_capacity(prepared.count);
    let mut offset = 0;
    let mut bytes = 0;
    while offset < prepared.count {
        let script = format!(
            "/* offset:{offset} */\n{}",
            include_str!("browser_chunk.js").replace("__OFFSET__", &offset.to_string())
        );
        let output = runner("eval", &[script])?;
        bytes += output.len();
        if bytes > 16 * 1024 * 1024 {
            return Err("browser transcript exceeded 16 MiB".into());
        }
        let chunk: Chunk = decode(&output)?;
        if chunk.offset != offset
            || chunk.next <= offset
            || chunk.next > prepared.count
            || chunk.next - offset != chunk.segments.len()
        {
            return Err("incomplete or inconsistent browser transcript chunk".into());
        }
        offset = chunk.next;
        segments.extend(chunk.segments);
    }
    let transcript = Transcript {
        video_id: id.into(),
        url,
        language: prepared.language,
        source: "browser".into(),
        generated: prepared.generated,
        segments,
    };
    transcript.validate()?;
    Ok(transcript)
}
