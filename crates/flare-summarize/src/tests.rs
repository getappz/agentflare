use super::*;
use serde_json::json;

fn transcript(text: &str) -> Transcript {
    Transcript {
        video_id: "dQw4w9WgXcQ".into(),
        url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into(),
        language: "en".into(),
        source: "fetch".into(),
        generated: Some(false),
        segments: vec![Segment {
            start: 3661.0,
            text: text.into(),
        }],
    }
}

#[test]
fn accepts_supported_youtube_urls_and_ids() {
    for input in [
        "dQw4w9WgXcQ",
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=1",
        "https://youtu.be/dQw4w9WgXcQ",
        "https://m.youtube.com/shorts/dQw4w9WgXcQ",
        "https://www.youtube.com/embed/dQw4w9WgXcQ",
        "https://www.youtube.com/live/dQw4w9WgXcQ",
    ] {
        assert_eq!(video_id(input).unwrap(), "dQw4w9WgXcQ");
    }
}

#[test]
fn rejects_untrusted_hosts_and_invalid_ids() {
    for input in [
        "https://youtube.com.evil.test/watch?v=dQw4w9WgXcQ",
        "https://evil.test/dQw4w9WgXcQ",
        "file:///dQw4w9WgXcQ",
        "bad",
        "https://youtube.com/watch?v=bad",
    ] {
        assert!(video_id(input).is_err(), "{input}");
    }
}

#[test]
fn renders_timestamps_and_rejects_empty_or_bad_segments() {
    assert!(transcript("Hello").render().contains("[01:01:01] Hello"));
    assert!(transcript("  ").validate().is_err());
    let mut bad = transcript("Hello");
    bad.segments[0].start = f64::NAN;
    assert!(bad.validate().is_err());
    bad.segments[0].start = -1.0;
    assert!(bad.validate().is_err());
}

#[tokio::test]
async fn direct_success_does_not_launch_browser() {
    let mut browser =
        |_: &str, _: &[String]| -> Result<String, String> { panic!("unexpected browser") };
    let result = extract_with(
        "dQw4w9WgXcQ",
        &["en".into()],
        async { Ok(transcript("Hello")) },
        Some(&mut browser),
    )
    .await
    .unwrap();
    assert_eq!(result.source, "fetch");
}

#[tokio::test]
async fn direct_wrong_language_is_rejected() {
    let mut fetched = transcript("Bonjour");
    fetched.language = "fr".into();
    let error = extract_with("dQw4w9WgXcQ", &["en".into()], async { Ok(fetched) }, None)
        .await
        .unwrap_err();
    assert!(error.contains("unrequested language"), "{error}");
}

#[tokio::test]
async fn fetch_only_reports_failure() {
    let error = extract_with(
        "dQw4w9WgXcQ",
        &["en".into()],
        async { Err("blocked".into()) },
        None,
    )
    .await
    .unwrap_err();
    assert!(error.contains("blocked"));
}

#[tokio::test]
async fn empty_languages_fail_before_polling_fetch() {
    let error = extract_with(
        "dQw4w9WgXcQ",
        &[],
        async { panic!("fetch should not run with invalid language options") },
        None,
    )
    .await
    .unwrap_err();
    assert!(error.contains("language"));
}

#[tokio::test]
async fn browser_wrong_language_is_rejected_and_closed() {
    let mut closed = false;
    let mut browser = |action: &str, _: &[String]| {
        if action == "close" {
            closed = true;
        }
        Ok(json!({"count":1,"language":"fr"}).to_string())
    };
    let error = extract_with(
        "dQw4w9WgXcQ",
        &["en".into()],
        async { Err("blocked".into()) },
        Some(&mut browser),
    )
    .await
    .unwrap_err();
    assert!(error.contains("unrequested language"));
    assert!(closed);
}

#[tokio::test]
async fn browser_accepts_case_insensitive_language_variants() {
    let mut browser = |action: &str, args: &[String]| {
        Ok(match action {
            "eval" if args[0].contains("/* prepare */") => {
                json!({"count":1,"language":"en-US"}).to_string()
            }
            "eval" => {
                json!({"offset":0,"next":1,"segments":[{"start":0.0,"text":"Hello"}]}).to_string()
            }
            _ => String::new(),
        })
    };
    let result = extract_with(
        "dQw4w9WgXcQ",
        &["EN-gb".into()],
        async { Err("blocked".into()) },
        Some(&mut browser),
    )
    .await
    .unwrap();
    assert_eq!(result.language, "en-US");
}

#[tokio::test]
async fn fallback_reads_every_chunk_and_closes_session() {
    let mut offsets = Vec::new();
    let mut closed = false;
    let mut browser = |action: &str, args: &[String]| -> Result<String, String> {
        if action == "open" {
            return Ok(String::new());
        }
        if action == "close" {
            closed = true;
            return Ok(String::new());
        }
        let js = &args[0];
        if js.contains("/* prepare */") {
            return Ok(json!({"count":3,"language":"en"}).to_string());
        }
        let offset = if js.contains("/* offset:0 */") { 0 } else { 2 };
        offsets.push(offset);
        Ok(if offset == 0 { json!({"offset":0,"next":2,"segments":[{"start":0.0,"text":"first"},{"start":1.0,"text":"second"}]}) } else { json!({"offset":2,"next":3,"segments":[{"start":2.0,"text":"last"}]}) }.to_string())
    };
    let result = extract_with(
        "dQw4w9WgXcQ",
        &["en".into()],
        async { Err("blocked".into()) },
        Some(&mut browser),
    )
    .await
    .unwrap();
    assert_eq!(result.segments.len(), 3);
    assert_eq!(result.segments[2].text, "last");
    assert_eq!(offsets, [0, 2]);
    assert!(closed);
    assert_eq!(result.source, "browser");
}

#[tokio::test]
async fn empty_fetch_triggers_fallback_and_preserves_both_errors() {
    let mut browser = |_: &str, _: &[String]| Err("browser unavailable".into());
    let error = extract_with(
        "dQw4w9WgXcQ",
        &["en".into()],
        async { Ok(transcript("")) },
        Some(&mut browser),
    )
    .await
    .unwrap_err();
    assert!(error.contains("empty"));
    assert!(error.contains("browser unavailable"));
}

#[tokio::test]
async fn browser_rejects_incomplete_chunks_and_still_closes() {
    let mut closed = false;
    let mut browser = |action: &str, args: &[String]| {
        if action == "close" {
            closed = true;
            return Ok(String::new());
        }
        if action == "open" {
            return Ok(String::new());
        }
        Ok(if args[0].contains("/* prepare */") {
            json!({"count":2,"language":"en"})
        } else {
            json!({"offset":0,"next":0,"segments":[]})
        }
        .to_string())
    };
    assert!(
        extract_with(
            "dQw4w9WgXcQ",
            &["en".into()],
            async { Err("blocked".into()) },
            Some(&mut browser)
        )
        .await
        .is_err()
    );
    assert!(closed);
}
