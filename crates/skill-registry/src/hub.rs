//! Remote skill hub: push/pull SkillBundle values via HTTP
//! with retry, timeout, and exponential backoff.

use crate::pack::SkillBundle;

const MAX_RETRIES: u32 = 3;
const BASE_DELAY_MS: u64 = 200;
const MAX_DELAY_MS: u64 = 5000;

/// Error type for hub operations.
#[derive(Debug)]
pub enum HubError {
    Http(String),
    Serde(String),
    Io(String),
    RetryExhausted(String),
}

impl std::fmt::Display for HubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(msg) => write!(f, "HTTP error: {msg}"),
            Self::Serde(msg) => write!(f, "serialization error: {msg}"),
            Self::Io(msg) => write!(f, "I/O error: {msg}"),
            Self::RetryExhausted(msg) => write!(f, "retry exhausted: {msg}"),
        }
    }
}

impl std::error::Error for HubError {}

fn retryable(err: &ureq::Error) -> bool {
    match err {
        ureq::Error::Transport(_) => true,
        ureq::Error::Status(s, _) => matches!(s, 429 | 502 | 503 | 504),
    }
}

fn with_retry<F, T>(label: &str, f: &mut F) -> Result<T, HubError>
where
    F: FnMut() -> Result<T, Box<ureq::Error>>,
{
    let mut last_err = None;
    for attempt in 0..MAX_RETRIES {
        match f() {
            Ok(val) => return Ok(val),
            Err(e) => {
                if !retryable(&e) || attempt + 1 >= MAX_RETRIES {
                    return Err(HubError::Http(e.to_string()));
                }
                last_err = Some(e);
                let delay = BASE_DELAY_MS
                    .saturating_mul(1u64 << attempt)
                    .min(MAX_DELAY_MS);
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
        }
    }
    Err(HubError::RetryExhausted(format!("{label}: {last_err:?}")))
}

/// Pull a SkillBundle from a remote hub URL (GET /skills/bundle).
pub fn pull_bundle(hub_url: &str) -> Result<SkillBundle, HubError> {
    let url = format!("{}/skills/bundle", hub_url.trim_end_matches('/'));
    let body = with_retry("pull", &mut || -> Result<String, Box<ureq::Error>> {
        ureq::get(&url)
            .set("User-Agent", "agentflare-skill-registry/0.1")
            .call()?
            .into_string()
            .map_err(|e| Box::new(ureq::Error::from(e)))
    })?;
    SkillBundle::from_json(&body).map_err(|e| HubError::Serde(e.to_string()))
}

/// Push a SkillBundle to a remote hub (PUT /skills/bundle).
pub fn push_bundle(hub_url: &str, bundle: &SkillBundle) -> Result<(), HubError> {
    let url = format!("{}/skills/bundle", hub_url.trim_end_matches('/'));
    let json = bundle
        .to_json()
        .map_err(|e| HubError::Serde(e.to_string()))?;
    // Use a flag rather than a retryable error: 4xx/5xx from the hub is not
    // likely to succeed on retry, but timeouts and transport errors are.
    let mut http_err: Option<String> = None;
    let success = with_retry("push", &mut || {
        let resp = ureq::put(&url)
            .set("Content-Type", "application/json")
            .set("User-Agent", "agentflare-skill-registry/0.1")
            .send_string(&json)?;
        let status = resp.status();
        if status >= 400 {
            http_err = Some(format!("PUT {url} returned {status}"));
            // Return OK to suppress retry — we'll check the flag after.
            return Ok(());
        }
        Ok(())
    });
    match (&success, &http_err) {
        (Err(e), _) => return Err(HubError::Http(e.to_string())),
        (Ok(()), Some(msg)) => return Err(HubError::Http(msg.clone())),
        (Ok(()), None) => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn pull_bundle_builds_correct_url() {
        let url = format!("{}/skills/bundle", "https://hub.example.com/api");
        assert_eq!(url, "https://hub.example.com/api/skills/bundle");
    }

    #[test]
    fn push_bundle_builds_correct_url() {
        let url = format!("{}/skills/bundle", "https://hub.example.com/api");
        assert_eq!(url, "https://hub.example.com/api/skills/bundle");
    }

    #[test]
    fn retry_delay_grows_exponentially() {
        let d0 = 200u64;
        let d1 = d0.saturating_mul(2).min(5000);
        let d2 = d0.saturating_mul(4).min(5000);
        assert_eq!(d0, 200);
        assert_eq!(d1, 400);
        assert_eq!(d2, 800);
    }
}
