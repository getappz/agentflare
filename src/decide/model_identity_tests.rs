use super::{Config, Question, client, provider::Provider, shadow, tracking};
use serde_json::json;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::{BufRead, Read, Write},
    net::TcpListener,
    time::Duration,
};

fn cfg(url: String, model: &str) -> Config {
    Config {
        provider: Provider::OpenRouter {
            key: "test-key".into(),
        },
        url_override: Some(url),
        model_override: Some(model.into()),
        timeout: Duration::from_secs(2),
    }
}

fn questions() -> BTreeMap<String, Question> {
    BTreeMap::from([("q".into(), Question::noul("public rubric"))])
}

#[test]
fn failed_requests_retain_the_requested_version_for_drift() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let questions = questions();
    let rows = RefCell::new(Vec::new());
    for model in ["jev-v1", "jev-v2"] {
        let error = client::ask_with(&cfg(url.clone(), model), &json!("state"), &questions)
            .err()
            .unwrap();
        tracking::failure(
            shadow::Row::failed("site", "state", "no", tracking::status(&error)),
            &questions,
            1,
            &|row| rows.borrow_mut().push(row.clone()),
        );
    }
    let rows = rows.borrow();
    let r = super::report::replay(&rows, &[], None);
    assert_eq!(r.models["site"]["jev-v1"].decisions, 1);
    assert_eq!(r.models["site"]["jev-v2"].decisions, 1);
    let first = serde_json::to_value(&rows[0]).unwrap();
    assert_eq!(first["requested_model"], "jev-v1");
    assert!(first["reported_model"].is_null());
}

#[test]
fn successful_response_model_is_distinct_from_requested_and_falls_back_when_missing() {
    for reported in [None, Some("resolved-v7")] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let body = json!({"answers":{"q":{"type":"noul","noul":0.9}},"model":reported}).to_string();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut reader = std::io::BufReader::new(s.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse::<usize>().unwrap();
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let questions = questions();
        let out = client::ask_with(&cfg(url, "requested-v3"), &json!("state"), &questions).unwrap();
        server.join().unwrap();
        let rows = RefCell::new(Vec::new());
        tracking::finish(
            shadow::Row::answered("site", "state", "no", "yes", None, 1, None),
            tracking::Context {
                questions: &questions,
                answers: &out.response.answers,
                source_model: out.response.model.as_deref(),
                usage: &out.response.usage,
                applied: true,
            },
            None,
            &|r| rows.borrow_mut().push(r.clone()),
        );
        let rows = rows.borrow();
        let row = serde_json::to_value(&rows[0]).unwrap();
        assert_eq!(row["source_model"], reported.unwrap_or("requested-v3"));
        assert_eq!(row["requested_model"], "requested-v3");
        assert_eq!(row["reported_model"], json!(reported));
        let captured = tracking::captured(
            rows[0].decision_id.as_deref().unwrap(),
            super::capture::Input {
                site: "site",
                features: json!({}),
                norm_input: "state",
                label: json!({"summary":"yes"}),
                confidence: None,
                baseline: "no",
                source_model: reported,
            },
            &questions,
            &out.response.answers,
        );
        assert_eq!(
            captured.source_model.as_deref(),
            Some(reported.unwrap_or("requested-v3"))
        );
    }
}
