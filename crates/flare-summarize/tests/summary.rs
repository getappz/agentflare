use flare_summarize::references::{collect_references, description_from_html};
use flare_summarize::summary::{Format, Length, SummaryOptions, render_transcript, summarize_with};
use flare_summarize::{Segment, Transcript};
use serde_json::json;

fn transcript() -> Transcript {
    Transcript {
        video_id: "dQw4w9WgXcQ".into(),
        url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into(),
        language: "en".into(),
        source: "fetch".into(),
        generated: Some(false),
        segments: vec![
            Segment {
                start: 0.0,
                text: "Use a reusable skill.".into(),
            },
            Segment {
                start: 30.5,
                text: "Review the result before deploying.".into(),
            },
        ],
    }
}

fn answer() -> String {
    json!({"overview":"Build and review a website.","takeaways":[{"text":"Review before deployment.","start":30.5}]}).to_string()
}

#[test]
fn summary_options_and_references_are_in_the_first_response() {
    let options = SummaryOptions {
        length: Length::Short,
        language: Some("French".into()),
        prompt: Some("Focus on deployment".into()),
    };
    let description = "Skill: https://example.com/skill.zip\nRepo: https://github.com/example/skill\nHost (affiliate): https://example.com/deal";
    let report = summarize_with(&transcript(), Some(description), &options, &mut |prompt| {
        assert!(prompt.contains("French"));
        assert!(prompt.contains("Focus on deployment"));
        assert!(prompt.contains("100 words"));
        assert!(prompt.contains("Review the result before deploying"));
        Ok(answer())
    })
    .unwrap();
    assert_eq!(report.references.len(), 3);
    let markdown = report.render(Format::Markdown).unwrap();
    assert!(markdown.contains("References and resources"));
    assert!(markdown.contains("https://github.com/example/skill"));
    assert!(markdown.contains("&t=30s"));
    let plain = report.render(Format::Text).unwrap();
    assert!(plain.contains("[00:00:30]"));
    assert!(!plain.contains("]("));
    let data: serde_json::Value =
        serde_json::from_str(&report.render(Format::Json).unwrap()).unwrap();
    assert_eq!(data["summary"]["takeaways"][0]["start"], 30.5);
    assert_eq!(data["references"].as_array().unwrap().len(), 3);
}

#[test]
fn short_summaries_rewrite_an_overshoot_once() {
    let mut calls = 0;
    let report = summarize_with(&transcript(), Some(""), &SummaryOptions { length: Length::Short, ..SummaryOptions::default() }, &mut |_| {
        calls += 1;
        if calls == 1 {
            Ok(json!({"overview":"Too many points", "takeaways":(0..8).map(|_| json!({"text":"A point", "start":0.0})).collect::<Vec<_>>()} ).to_string())
        } else { Ok(answer()) }
    }).unwrap();
    assert_eq!(calls, 2);
    assert!(report.summary.takeaways.len() <= 3);
    let too_long =
        json!({"overview":"word ".repeat(200), "takeaways":[{"text":"Point", "start":0.0}]})
            .to_string();
    let mut calls = 0;
    let result = summarize_with(
        &transcript(),
        Some(""),
        &SummaryOptions {
            length: Length::Short,
            ..SummaryOptions::default()
        },
        &mut |_| {
            calls += 1;
            Ok(too_long.clone())
        },
    );
    assert!(result.is_err());
    assert_eq!(calls, 2);
}

#[test]
fn missing_description_is_disclosed_and_bad_model_output_fails() {
    let report = summarize_with(&transcript(), None, &SummaryOptions::default(), &mut |_| {
        Ok(answer())
    })
    .unwrap();
    assert!(report.warnings.iter().any(|s| s.contains("description")));
    assert!(
        report
            .render(Format::Markdown)
            .unwrap()
            .contains("could not be checked")
    );
    assert!(
        summarize_with(
            &transcript(),
            None,
            &SummaryOptions::default(),
            &mut |_| Ok("made up prose".into())
        )
        .is_err()
    );
    let wrong_time =
        json!({"overview":"Summary","takeaways":[{"text":"Invented timestamp","start":9999}]})
            .to_string();
    assert!(
        summarize_with(
            &transcript(),
            None,
            &SummaryOptions::default(),
            &mut |_| Ok(wrong_time.clone())
        )
        .is_err()
    );
}

#[test]
fn long_transcripts_cover_all_chunks_before_final_summary() {
    let mut source = transcript();
    source.segments = (0..1200)
        .map(|i| Segment {
            start: i as f64,
            text: format!("UNIQUE_{i}: {}", "Caption content. ".repeat(4)),
        })
        .collect();
    let mut prompts = Vec::new();
    let report = summarize_with(
        &source,
        Some(""),
        &SummaryOptions::default(),
        &mut |prompt| {
            prompts.push(prompt.to_owned());
            Ok(json!({"overview":"Notes","takeaways":[{"text":"Point","start":0.0}]}).to_string())
        },
    )
    .unwrap();
    assert!(prompts.len() > 2);
    assert!(prompts.iter().any(|p| p.contains("UNIQUE_0:")));
    assert!(prompts.iter().any(|p| p.contains("UNIQUE_1199:")));
    assert_eq!(report.transcript.segments.len(), 1200);
}

#[test]
fn transcript_formats_preserve_times_only_when_requested() {
    let source = transcript();
    assert!(
        render_transcript(&source, Format::Markdown, true)
            .unwrap()
            .contains("&t=30s")
    );
    assert!(
        !render_transcript(&source, Format::Text, false)
            .unwrap()
            .contains("[00:")
    );
    let data: serde_json::Value =
        serde_json::from_str(&render_transcript(&source, Format::Json, false).unwrap()).unwrap();
    assert_eq!(data["segments"][1]["start"], 30.5);
}

#[test]
fn description_json_decoding_and_reference_dedup_preserve_urls() {
    let html = r#"<script>var ytInitialPlayerResponse = {"videoDetails":{"shortDescription":"Skill: https://example.com/file?x=1\u0026y=2\nRepo: https://github.com/example/repo"}}; unrelated();</script>"#;
    let description = description_from_html(html).unwrap();
    assert!(description.contains("?x=1&y=2"));
    let refs = collect_references(Some(&description), &transcript());
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].source, "description");
    assert!(description_from_html("<html>footer</html>").is_none());
    let repeated = format!("{description}\nAgain: https://github.com/example/repo");
    assert_eq!(collect_references(Some(&repeated), &transcript()).len(), 2);
}

#[test]
fn unicode_caption_larger_than_a_chunk_is_fully_processed() {
    let mut source = transcript();
    source.segments = vec![Segment {
        start: 0.0,
        text: "界".repeat(50_000) + "FINAL_MARKER",
    }];
    let mut characters_seen = 0;
    let mut saw_end = false;
    summarize_with(
        &source,
        Some(""),
        &SummaryOptions::default(),
        &mut |prompt| {
            characters_seen += prompt.matches('界').count();
            saw_end |= prompt.contains("FINAL_MARKER");
            Ok(json!({"overview":"Notes","takeaways":[{"text":"Point","start":0.0}]}).to_string())
        },
    )
    .unwrap();
    assert_eq!(characters_seen, 50_000);
    assert!(saw_end);
}

#[test]
fn nonshrinking_notes_stop_instead_of_looping_or_truncating() {
    let mut source = transcript();
    source.segments[0].text = "a".repeat(20_000);
    source.segments[1].text = "b".repeat(20_000);
    let mut calls = 0;
    let error = summarize_with(&source, Some(""), &SummaryOptions::default(), &mut |_| {
        calls += 1;
        Ok(
            json!({"overview":"x".repeat(30_000),"takeaways":[{"text":"Point","start":0.0}]})
                .to_string(),
        )
    })
    .unwrap_err();
    assert_eq!(calls, 2);
    assert!(error.contains("did not shrink"));
}

#[test]
fn description_browser_fallback_decodes_result_and_closes_its_session() {
    let mut closed = false;
    let mut runner = |action: &str, _: &[String]| {
        if action == "close" {
            closed = true;
        }
        Ok(if action == "eval" {
            serde_json::to_string(
                &json!({"description":"Skill: https://example.com/skill"}).to_string(),
            )
            .unwrap()
        } else {
            String::new()
        })
    };
    let description =
        flare_summarize::references::browser_description("dQw4w9WgXcQ", &mut runner).unwrap();
    assert!(description.contains("https://example.com/skill"));
    assert!(closed);
}
