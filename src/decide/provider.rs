//! Provider selection and wire format. Pure functions (no I/O) so the request
//! and response shapes are unit-testable. Credentials deliberately have no
//! `Debug` impl anywhere in this file.
use super::types::{Question, Response};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

const OPENROUTER_URL: &str = "https://openrouter.ai/api/alpha/decisions";
const OPENROUTER_MODEL: &str = "typesafe/jev-1.13";
const CLOUDFLARE_MODEL: &str = "typesafe/jev";
const DEFAULT_TIMEOUT_MS: u64 = 3000;

#[derive(Debug, thiserror::Error)]
pub enum DecideError {
    #[error("decisions are disabled (set AGENTFLARE_JEV=1)")]
    Disabled,
    #[error(
        "no decision-backend credentials (OPENROUTER_API_KEY, or CLOUDFLARE_API_TOKEN + CLOUDFLARE_ACCOUNT_ID)"
    )]
    NoCredentials,
    #[error("decision request failed: {0}")]
    Transport(String),
    #[error("decision backend returned HTTP {0}: {1}")]
    Status(u16, String),
    #[error("decision response unusable: {0}")]
    Malformed(String),
}

pub enum Provider {
    OpenRouter {
        key: String,
    },
    Cloudflare {
        token: String,
        account: String,
        gateway: Option<String>,
    },
}

pub struct Config {
    pub provider: Provider,
    /// Replaces the provider URL entirely (proxies, tests).
    pub url_override: Option<String>,
    pub model_override: Option<String>,
    pub timeout: Duration,
}

pub struct HttpRequest {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Value,
}

impl Config {
    /// Build from an env-style lookup. Opt-in: `AGENTFLARE_JEV=1` is required.
    /// `AGENTFLARE_JEV_PROVIDER` = `openrouter` | `cloudflare` pins a provider;
    /// otherwise OpenRouter wins when its key is present, else Cloudflare.
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> Result<Self, DecideError> {
        if get("AGENTFLARE_JEV").as_deref() != Some("1") {
            return Err(DecideError::Disabled);
        }
        let val = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        // Lazy: a credential lookup may hit the vault, so only look up the
        // providers actually needed.
        let openrouter = || val("OPENROUTER_API_KEY").map(|key| Provider::OpenRouter { key });
        let cloudflare = || {
            val("CLOUDFLARE_API_TOKEN")
                .zip(val("CLOUDFLARE_ACCOUNT_ID"))
                .map(|(token, account)| Provider::Cloudflare {
                    token,
                    account,
                    gateway: val("CLOUDFLARE_AI_GATEWAY_ID"),
                })
        };
        let provider = match val("AGENTFLARE_JEV_PROVIDER").as_deref() {
            Some("openrouter") => openrouter(),
            Some("cloudflare") => cloudflare(),
            Some(_) => None,
            None => openrouter().or_else(cloudflare),
        }
        .ok_or(DecideError::NoCredentials)?;
        Ok(Self {
            provider,
            url_override: val("AGENTFLARE_JEV_BASE_URL"),
            model_override: val("AGENTFLARE_JEV_MODEL"),
            timeout: Duration::from_millis(
                val("AGENTFLARE_JEV_TIMEOUT_MS")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(DEFAULT_TIMEOUT_MS),
            ),
        })
    }

    pub fn request(&self, state: &Value, questions: &BTreeMap<String, Question>) -> HttpRequest {
        let model = |default: &str| {
            self.model_override
                .clone()
                .unwrap_or_else(|| default.to_string())
        };
        match &self.provider {
            Provider::OpenRouter { key } => HttpRequest {
                url: self
                    .url_override
                    .clone()
                    .unwrap_or_else(|| OPENROUTER_URL.to_string()),
                headers: vec![("Authorization", format!("Bearer {key}"))],
                body: json!({
                    "model": model(OPENROUTER_MODEL),
                    "state": state,
                    "questions": questions,
                }),
            },
            Provider::Cloudflare {
                token,
                account,
                gateway,
            } => {
                let mut headers = vec![("Authorization", format!("Bearer {token}"))];
                if let Some(gw) = gateway {
                    headers.push(("cf-aig-gateway-id", gw.clone()));
                }
                HttpRequest {
                    url: self.url_override.clone().unwrap_or_else(|| {
                        format!("https://api.cloudflare.com/client/v4/accounts/{account}/ai/run")
                    }),
                    headers,
                    body: json!({
                        "model": model(CLOUDFLARE_MODEL),
                        "input": { "state": state, "questions": questions },
                    }),
                }
            }
        }
    }
}

/// Parse a response body. Cloudflare's REST API wraps the payload in
/// `{result, success}`; OpenRouter returns it bare.
pub fn parse_response(body: &str) -> Result<Response, DecideError> {
    let mut v: Value = serde_json::from_str(body)
        .map_err(|_| DecideError::Malformed("body is not JSON".to_string()))?;
    if v.get("success").is_some()
        && let Some(result) = v.get_mut("result").map(Value::take)
    {
        v = result;
    }
    serde_json::from_value(v).map_err(|e| DecideError::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decide::Answer;

    fn lookup(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| v.to_string())
        }
    }

    fn qs() -> BTreeMap<String, Question> {
        BTreeMap::from([("urgent".to_string(), Question::noul("Is it urgent?"))])
    }

    #[test]
    fn disabled_unless_opted_in() {
        let get = lookup(&[("OPENROUTER_API_KEY", "k")]);
        assert!(matches!(
            Config::from_lookup(&get).err(),
            Some(DecideError::Disabled)
        ));
    }

    #[test]
    fn no_credentials_when_enabled_but_empty() {
        let get = lookup(&[("AGENTFLARE_JEV", "1"), ("OPENROUTER_API_KEY", "  ")]);
        assert!(matches!(
            Config::from_lookup(&get).err(),
            Some(DecideError::NoCredentials)
        ));
    }

    #[test]
    fn openrouter_wins_when_both_present_and_provider_can_be_pinned() {
        let both = &[
            ("AGENTFLARE_JEV", "1"),
            ("OPENROUTER_API_KEY", "k"),
            ("CLOUDFLARE_API_TOKEN", "t"),
            ("CLOUDFLARE_ACCOUNT_ID", "a"),
        ];
        let cfg = Config::from_lookup(&lookup(both)).ok().unwrap();
        assert!(matches!(cfg.provider, Provider::OpenRouter { .. }));

        let pinned = &[
            ("AGENTFLARE_JEV", "1"),
            ("AGENTFLARE_JEV_PROVIDER", "cloudflare"),
            ("OPENROUTER_API_KEY", "k"),
            ("CLOUDFLARE_API_TOKEN", "t"),
            ("CLOUDFLARE_ACCOUNT_ID", "a"),
        ];
        let cfg = Config::from_lookup(&lookup(pinned)).ok().unwrap();
        assert!(matches!(cfg.provider, Provider::Cloudflare { .. }));
    }

    #[test]
    fn unknown_or_uncredentialed_pinned_provider_is_no_credentials() {
        for name in ["vercel", "cloudflare"] {
            let get = move |k: &str| match k {
                "AGENTFLARE_JEV" => Some("1".to_string()),
                "AGENTFLARE_JEV_PROVIDER" => Some(name.to_string()),
                "OPENROUTER_API_KEY" => Some("k".to_string()),
                _ => None,
            };
            assert!(matches!(
                Config::from_lookup(&get).err(),
                Some(DecideError::NoCredentials)
            ));
        }
    }

    #[test]
    fn openrouter_request_shape() {
        let get = lookup(&[("AGENTFLARE_JEV", "1"), ("OPENROUTER_API_KEY", "sk-x")]);
        let req = Config::from_lookup(&get)
            .ok()
            .unwrap()
            .request(&json!("hello"), &qs());
        assert_eq!(req.url, OPENROUTER_URL);
        assert_eq!(
            req.headers,
            vec![("Authorization", "Bearer sk-x".to_string())]
        );
        assert_eq!(req.body["model"], "typesafe/jev-1.13");
        assert_eq!(req.body["state"], "hello");
        assert_eq!(req.body["questions"]["urgent"]["type"], "noul");
    }

    #[test]
    fn cloudflare_request_shape_with_gateway_header() {
        let get = lookup(&[
            ("AGENTFLARE_JEV", "1"),
            ("CLOUDFLARE_API_TOKEN", "tok"),
            ("CLOUDFLARE_ACCOUNT_ID", "acct"),
            ("CLOUDFLARE_AI_GATEWAY_ID", "gw"),
        ]);
        let req = Config::from_lookup(&get)
            .ok()
            .unwrap()
            .request(&json!("hello"), &qs());
        assert_eq!(
            req.url,
            "https://api.cloudflare.com/client/v4/accounts/acct/ai/run"
        );
        assert!(
            req.headers
                .contains(&("cf-aig-gateway-id", "gw".to_string()))
        );
        assert_eq!(req.body["model"], "typesafe/jev");
        assert_eq!(req.body["input"]["state"], "hello");
    }

    #[test]
    fn overrides_apply() {
        let get = lookup(&[
            ("AGENTFLARE_JEV", "1"),
            ("OPENROUTER_API_KEY", "k"),
            ("AGENTFLARE_JEV_BASE_URL", "http://127.0.0.1:9/x"),
            ("AGENTFLARE_JEV_MODEL", "typesafe/jev-latest"),
            ("AGENTFLARE_JEV_TIMEOUT_MS", "250"),
        ]);
        let cfg = Config::from_lookup(&get).ok().unwrap();
        assert_eq!(cfg.timeout, Duration::from_millis(250));
        let req = cfg.request(&json!(1), &qs());
        assert_eq!(req.url, "http://127.0.0.1:9/x");
        assert_eq!(req.body["model"], "typesafe/jev-latest");
    }

    const OPENROUTER_BODY: &str = r#"{"id":"gen-dec-1","model":"typesafe/jev-1.13-20260917","provider":"TypeSafe",
        "answers":{
          "is_bug":{"type":"noul","noul":0.96},
          "team":{"type":"choice","choice":"payments","confidence":0.75,"probabilities":{"payments":0.84,"frontend":0.16}},
          "urgency":{"type":"score","score":1.99,"confidence":0.99,"probabilities":{"2":0.99},"legend":{"2":"now"}}},
        "usage":{"input_tokens":476,"output_tokens":70,"cost":0.000019992}}"#;

    #[test]
    fn parses_openrouter_body() {
        let r = parse_response(OPENROUTER_BODY).ok().unwrap();
        assert_eq!(r.usage.input_tokens, 476);
        assert_eq!(r.usage.cost, Some(0.000019992));
        assert!(matches!(r.answers["is_bug"], Answer::Noul { noul } if noul == 0.96));
        assert_eq!(r.answers["team"].confidence(), Some(0.75));
        assert_eq!(r.answers["is_bug"].confidence(), None);
    }

    #[test]
    fn parses_cloudflare_wrapped_body_without_cost() {
        let body = format!(
            r#"{{"success":true,"errors":[],"result":{}}}"#,
            r#"{"model":"jev-1.13.0","answers":{"d":{"type":"choice","choice":"billing","probabilities":{"billing":1}}},"usage":{"input_tokens":380,"output_tokens":45}}"#
        );
        let r = parse_response(&body).ok().unwrap();
        assert_eq!(r.usage.cost, None);
        // confidence absent on the wire => None, never zero
        assert_eq!(r.answers["d"].confidence(), None);
        assert_eq!(r.answers["d"].summary(), "choice=billing");
    }

    #[test]
    fn rejects_garbage_and_unknown_answer_types() {
        assert!(matches!(
            parse_response("<html>").err(),
            Some(DecideError::Malformed(_))
        ));
        assert!(matches!(
            parse_response(r#"{"answers":{"x":{"type":"haiku"}}}"#).err(),
            Some(DecideError::Malformed(_))
        ));
    }
}
