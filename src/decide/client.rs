use super::provider::{Config, DecideError, parse_response};
use super::types::{Question, Response};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Instant;

pub struct Outcome {
    pub response: Response,
    pub elapsed_ms: u64,
}

pub struct Batch {
    state: Value,
    questions: BTreeMap<String, Question>,
}

impl Batch {
    pub fn new(state: impl Into<Value>) -> Self {
        Self {
            state: state.into(),
            questions: BTreeMap::new(),
        }
    }

    pub fn add(&mut self, id: impl Into<String>, question: Question) {
        self.questions.insert(id.into(), question);
    }

    pub fn is_empty(&self) -> bool {
        self.questions.is_empty()
    }

    pub fn questions(&self) -> &BTreeMap<String, Question> {
        &self.questions
    }

    pub fn ask_within(&self, cap: std::time::Duration) -> Result<Outcome, DecideError> {
        ask_within(&self.state, &self.questions, cap)
    }
}

/// Settings that are credentials: the only names looked up in the vault.
const VAULT_KEYS: &[&str] = &[
    "OPENROUTER_API_KEY",
    "CLOUDFLARE_API_TOKEN",
    "CLOUDFLARE_ACCOUNT_ID",
    "CLOUDFLARE_AI_GATEWAY_ID",
];

/// Layered lookup: process env, then the agentflare vault (credential names
/// only, same name as the env var; a locked vault just yields `None`), then
/// `~/.env`. The last layer keeps hooks and daemons working that don't inherit
/// an interactive shell's environment.
fn layered_lookup(
    env: impl Fn(&str) -> Option<String>,
    vault: impl Fn(&str) -> Option<String>,
    dotenv: Vec<(String, String)>,
) -> impl Fn(&str) -> Option<String> {
    let find = layered_source(env, vault, dotenv);
    move |k| find(k).map(|(value, _)| value)
}

/// Which layer a setting came from. Shown by `decide ping` so a user can see
/// where credentials are resolved without ever printing a value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Source {
    Env,
    Vault,
    Dotenv,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::Env => "environment",
            Self::Vault => "vault",
            Self::Dotenv => "~/.env",
        }
    }
}

fn layered_source(
    env: impl Fn(&str) -> Option<String>,
    vault: impl Fn(&str) -> Option<String>,
    dotenv: Vec<(String, String)>,
) -> impl Fn(&str) -> Option<(String, Source)> {
    move |k| {
        env(k)
            .map(|v| (v, Source::Env))
            .or_else(|| {
                VAULT_KEYS
                    .contains(&k)
                    .then(|| vault(k))
                    .flatten()
                    .map(|v| (v, Source::Vault))
            })
            .or_else(|| {
                dotenv
                    .iter()
                    .find(|(name, _)| name == k)
                    .map(|(_, v)| (v.clone(), Source::Dotenv))
            })
    }
}

fn real_source_lookup() -> impl Fn(&str) -> Option<(String, Source)> {
    let dotenv = dirs::home_dir()
        .and_then(|h| std::fs::read_to_string(h.join(".env")).ok())
        .map(|c| crate::dev_vars::parse(&c))
        .unwrap_or_default();
    layered_source(
        |k| std::env::var(k).ok(),
        |k| {
            // Session-only: the passphrase KDF must never run inside a hook.
            crate::vault::get_secret_session_only(k)
                .ok()
                .flatten()
                .map(|v| v.to_string())
        },
        dotenv,
    )
}

fn env_lookup() -> impl Fn(&str) -> Option<String> {
    let find = real_source_lookup();
    move |k| find(k).map(|(value, _)| value)
}

/// For each credential setting that resolves, the layer it resolved from
/// (names and layers only, never values).
pub fn credential_sources() -> Vec<(&'static str, Source)> {
    let find = real_source_lookup();
    VAULT_KEYS
        .iter()
        .filter_map(|k| find(k).map(|(_, source)| (*k, source)))
        .collect()
}

/// Ask the configured backend. Errors are meant to be swallowed by callers
/// (fail open): they never contain credentials or the request URL.
pub fn ask(state: &Value, questions: &BTreeMap<String, Question>) -> Result<Outcome, DecideError> {
    ask_with(&Config::from_lookup(&env_lookup())?, state, questions)
}

/// `ask`, with the timeout capped, for callers on a latency budget (hooks).
pub fn ask_within(
    state: &Value,
    questions: &BTreeMap<String, Question>,
    cap: std::time::Duration,
) -> Result<Outcome, DecideError> {
    let mut cfg = Config::from_lookup(&env_lookup())?;
    cfg.timeout = cfg.timeout.min(cap);
    ask_with(&cfg, state, questions)
}

pub fn ask_with(
    cfg: &Config,
    state: &Value,
    questions: &BTreeMap<String, Question>,
) -> Result<Outcome, DecideError> {
    let req = cfg.request(state, questions);
    let agent = ureq::AgentBuilder::new().timeout(cfg.timeout).build();
    let mut call = agent.post(&req.url);
    for (name, value) in &req.headers {
        call = call.set(name, value);
    }
    let start = Instant::now();
    // ureq returns non-2xx as `Err(Status(..))`; keep the body for the message.
    let (status, body) = match call.send_json(&req.body) {
        Ok(resp) => (resp.status(), resp.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, resp)) => (code, resp.into_string().unwrap_or_default()),
        Err(e) => return Err(DecideError::Transport(describe_transport(&e))),
    };
    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    if !(200..300).contains(&status) {
        return Err(DecideError::Status(
            status,
            body.chars().take(200).collect(),
        ));
    }
    Ok(Outcome {
        response: parse_response(&body)?,
        elapsed_ms,
    })
}

/// URL-free description of a transport failure: `ureq::Error`'s `Display`
/// prepends the request URL, and error strings reach logs. Same approach as
/// `channels::describe_send_error`.
fn describe_transport(err: &ureq::Error) -> String {
    let mut msg = err.kind().to_string();
    if let ureq::Error::Transport(t) = err {
        if let Some(detail) = t.message() {
            msg.push_str(": ");
            msg.push_str(detail);
        }
        if let Some(source) = std::error::Error::source(t) {
            msg.push_str(": ");
            msg.push_str(&source.to_string());
        }
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decide::Answer;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    const KEY: &str = "sk-or-SECRET-KEY";

    /// One-shot fake server: reads the whole request, then writes `reply`
    /// (or stalls for `stall` first). Returns the URL to post to.
    fn serve_once(reply: String, stall: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/decisions", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let Ok((mut s, _)) = listener.accept() else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = s.read(&mut chunk).unwrap_or(0);
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let len = text[..head_end]
                        .to_lowercase()
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:").map(str::to_owned))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if buf.len() >= head_end + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            std::thread::sleep(stall);
            let _ = s.write_all(reply.as_bytes());
        });
        url
    }

    fn http(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn cfg(url: String, timeout_ms: &str) -> Config {
        let get = move |k: &str| match k {
            "AGENTFLARE_JEV" => Some("1".to_string()),
            "OPENROUTER_API_KEY" => Some(KEY.to_string()),
            "AGENTFLARE_JEV_BASE_URL" => Some(url.clone()),
            "AGENTFLARE_JEV_TIMEOUT_MS" => Some(timeout_ms.to_string()),
            _ => None,
        };
        Config::from_lookup(&get).ok().unwrap()
    }

    fn questions() -> BTreeMap<String, Question> {
        BTreeMap::from([("urgent".to_string(), Question::noul("Is it urgent?"))])
    }

    fn dotenv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn lookup_order_is_env_then_vault_then_dotenv() {
        let get = layered_lookup(
            |k| (k == "OPENROUTER_API_KEY").then(|| "from-env".to_string()),
            |k| match k {
                "OPENROUTER_API_KEY" => Some("from-vault".to_string()),
                "CLOUDFLARE_API_TOKEN" => Some("cf-vault".to_string()),
                _ => None,
            },
            dotenv(&[
                ("OPENROUTER_API_KEY", "from-dotenv"),
                ("CLOUDFLARE_API_TOKEN", "cf-dotenv"),
                ("CLOUDFLARE_ACCOUNT_ID", "acct-dotenv"),
            ]),
        );
        assert_eq!(get("OPENROUTER_API_KEY").as_deref(), Some("from-env"));
        assert_eq!(get("CLOUDFLARE_API_TOKEN").as_deref(), Some("cf-vault"));
        assert_eq!(get("CLOUDFLARE_ACCOUNT_ID").as_deref(), Some("acct-dotenv"));
        assert_eq!(get("NOT_SET"), None);
    }

    #[test]
    fn source_reports_the_winning_layer() {
        let find = layered_source(
            |k| (k == "OPENROUTER_API_KEY").then(|| "e".to_string()),
            |k| (k == "CLOUDFLARE_API_TOKEN").then(|| "v".to_string()),
            dotenv(&[("CLOUDFLARE_ACCOUNT_ID", "d")]),
        );
        assert_eq!(find("OPENROUTER_API_KEY").unwrap().1, Source::Env);
        assert_eq!(find("CLOUDFLARE_API_TOKEN").unwrap().1, Source::Vault);
        assert_eq!(find("CLOUDFLARE_ACCOUNT_ID").unwrap().1, Source::Dotenv);
        assert!(find("CLOUDFLARE_AI_GATEWAY_ID").is_none());
    }

    #[test]
    fn locked_vault_falls_through_and_only_credentials_hit_the_vault() {
        let get = layered_lookup(
            |_| None,
            |k| {
                assert!(VAULT_KEYS.contains(&k), "vault consulted for {k}");
                None // locked
            },
            dotenv(&[
                ("OPENROUTER_API_KEY", "from-dotenv"),
                ("AGENTFLARE_JEV", "1"),
            ]),
        );
        assert_eq!(get("OPENROUTER_API_KEY").as_deref(), Some("from-dotenv"));
        assert_eq!(get("AGENTFLARE_JEV").as_deref(), Some("1")); // non-credential: no vault call
    }

    #[test]
    fn success_returns_typed_answers() {
        let body = r#"{"model":"m","answers":{"urgent":{"type":"noul","noul":0.9}},"usage":{"input_tokens":5,"output_tokens":2}}"#;
        let url = serve_once(http("200 OK", body), Duration::ZERO);
        let out = ask_with(&cfg(url, "3000"), &Value::from("help"), &questions())
            .ok()
            .unwrap();
        assert!(matches!(out.response.answers["urgent"], Answer::Noul { noul } if noul == 0.9));
        assert_eq!(out.response.usage.input_tokens, 5);
    }

    #[test]
    fn http_error_is_a_status_error_without_the_key() {
        let url = serve_once(
            http(
                "402 Payment Required",
                r#"{"error":{"message":"Insufficient credits"}}"#,
            ),
            Duration::ZERO,
        );
        let err = ask_with(&cfg(url, "3000"), &Value::from("x"), &questions())
            .err()
            .unwrap();
        assert!(matches!(err, DecideError::Status(402, _)));
        assert!(!err.to_string().contains(KEY));
    }

    #[test]
    fn stalled_server_times_out_as_transport_error_without_url_or_key() {
        let url = serve_once(http("200 OK", "{}"), Duration::from_secs(3));
        let err = ask_with(&cfg(url.clone(), "200"), &Value::from("x"), &questions())
            .err()
            .unwrap();
        let msg = err.to_string();
        assert!(matches!(err, DecideError::Transport(_)), "{msg}");
        assert!(!msg.contains(KEY) && !msg.contains(&url), "{msg}");
    }

    #[test]
    fn refused_connection_is_a_transport_error() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("http://127.0.0.1:{port}/decisions"); // listener dropped => refused
        let err = ask_with(&cfg(url, "500"), &Value::from("x"), &questions())
            .err()
            .unwrap();
        assert!(matches!(err, DecideError::Transport(_)));
    }

    #[test]
    fn non_json_body_is_malformed() {
        let url = serve_once(http("200 OK", "<html>nope</html>"), Duration::ZERO);
        let err = ask_with(&cfg(url, "3000"), &Value::from("x"), &questions())
            .err()
            .unwrap();
        assert!(matches!(err, DecideError::Malformed(_)));
    }
}
