//! The single HTTP call site. Attaches auth + GitHub headers, refuses writes
//! with no credential, maps status codes to `GitHubError`, and applies
//! GitHub's published client etiquette process-wide: a backoff deadline after
//! any rate limit (primary or secondary) during which no call is made at all,
//! serialized, spaced-out mutating requests, and conditional GETs
//! (`If-None-Match` from a per-host ETag cache, so an unchanged resource is
//! a 304 that costs no rate-limit budget instead of a re-download).

use crate::github::GitHubError;
use crate::github::auth;
use crate::github::etag_cache::{CachedGet, EtagCache};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// `Clone` is cheap: `ureq::Agent` wraps an `Arc` internally and all clones
/// share its connection pool, so the bridge daemon can build one `Client`
/// per credential resolution and reuse it across every repo it polls
/// instead of re-authenticating per repo per tick.
#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    token: Option<String>,
    base_url: String,
    throttle: Arc<HostThrottle>,
    /// Minimum gap between two mutating requests to the same host.
    mutation_spacing: Duration,
    /// Whether a 401 may drop the cached credential and retry once with a
    /// freshly resolved one -- only for clients built from [`auth`]'s own
    /// resolution, never for an explicit test token.
    refresh_on_401: bool,
}

const BASE_URL: &str = "https://api.github.com";

/// Max items per page GitHub allows for the list endpoints used here.
const PER_PAGE: usize = 100;

/// GitHub's guidance for avoiding secondary rate limits: make mutating
/// requests serially, at least a second apart.
const MUTATION_SPACING: Duration = Duration::from_secs(1);

/// Backoff applied to a secondary limit (or a bare 429) that names no
/// explicit wait -- GitHub's docs say to wait "at least one minute".
const DEFAULT_SECONDARY_BACKOFF: Duration = Duration::from_secs(60);

/// Per-host throttle state and ETag cache, shared by every `Client` in the
/// process aimed at the same base URL. Keyed by host rather than truly
/// global so the test binary's many concurrent mock servers (one per
/// ephemeral port) never throttle each other or see each other's cached
/// responses; production only ever talks to one host, so there it is
/// effectively process-wide.
struct HostThrottle {
    /// No request to this host before this instant (a rate-limit backoff).
    backoff_until: Mutex<Option<Instant>>,
    /// Held for the whole of a mutating request, so writes go out one at a
    /// time; stores when the previous one finished, for spacing.
    last_mutation: Mutex<Option<Instant>>,
    /// Validators and bodies of past GETs, for conditional requests.
    etags: Mutex<EtagCache>,
}

fn host_throttle(base_url: &str) -> Arc<HostThrottle> {
    static HOSTS: OnceLock<Mutex<HashMap<String, Arc<HostThrottle>>>> = OnceLock::new();
    let mut hosts = HOSTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    Arc::clone(hosts.entry(base_url.to_string()).or_insert_with(|| {
        Arc::new(HostThrottle {
            backoff_until: Mutex::new(None),
            last_mutation: Mutex::new(None),
            etags: Mutex::new(EtagCache::default()),
        })
    }))
}

/// The rate-limit headers GitHub attaches to an error response.
#[derive(Debug, Default)]
struct RateHeaders {
    remaining: Option<String>,
    /// `x-ratelimit-reset`: epoch seconds the primary window resets at.
    reset: Option<String>,
    /// `retry-after`: seconds to wait (GitHub sends the delta-seconds form).
    retry_after: Option<String>,
}

fn is_mutating(method: &str) -> bool {
    matches!(method, "POST" | "PATCH" | "PUT" | "DELETE")
}

/// Whether a `/graphql` request body carries a mutation (as opposed to a
/// query): GraphQL documents start with their operation type, and every
/// mutation agentflare sends spells it out.
fn is_graphql_mutation(body: &Option<serde_json::Value>) -> bool {
    body.as_ref()
        .and_then(|b| b["query"].as_str())
        .is_some_and(|q| q.trim_start().starts_with("mutation"))
}

/// GitHub's secondary ("abuse") limits come back as a 403 or 429 carrying
/// either a `retry-after` header or a message naming the limit, and --
/// unlike the primary limit -- usually with `x-ratelimit-remaining` well
/// above zero, which is why checking only that header missed them.
fn is_secondary_limit(status: u16, headers: &RateHeaders, body: &str) -> bool {
    if !matches!(status, 403 | 429) {
        return false;
    }
    let lower = body.to_lowercase();
    headers.retry_after.is_some()
        || lower.contains("secondary rate limit")
        || lower.contains("abuse")
}

fn map_status(status: u16, headers: &RateHeaders, body: String) -> GitHubError {
    match status {
        401 => GitHubError::NoAuth(
            "GitHub rejected the credential (401). Refresh it: 'gh auth login' or reset GITHUB_TOKEN.".to_string(),
        ),
        403 | 429 if is_secondary_limit(status, headers, &body) => GitHubError::RateLimited(
            format!("GitHub secondary rate limit hit ({status}); backing off before retrying."),
        ),
        403 if headers.remaining.as_deref() == Some("0") => GitHubError::RateLimited(
            "GitHub rate limit hit. Authenticate to raise it to 5000 req/hr.".to_string(),
        ),
        403 => GitHubError::Forbidden(
            "GitHub returned 403 — the token lacks the required scope/permission.".to_string(),
        ),
        404 => GitHubError::NotFound,
        429 => GitHubError::RateLimited("GitHub rate limit hit (429).".to_string()),
        _ => GitHubError::Http { status, body },
    }
}

/// How long to stop calling GitHub after an error response, if at all:
/// `retry-after` when present, else the primary window's reset time when
/// the primary budget is exhausted, else a default wait for a secondary
/// limit or a bare 429. `now_epoch` is injected for tests.
fn backoff_for(status: u16, headers: &RateHeaders, body: &str, now_epoch: u64) -> Option<Duration> {
    if !matches!(status, 403 | 429) {
        return None;
    }
    if let Some(secs) = headers
        .retry_after
        .as_deref()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        return Some(Duration::from_secs(secs.max(1)));
    }
    if headers.remaining.as_deref() == Some("0")
        && let Some(reset) = headers
            .reset
            .as_deref()
            .and_then(|v| v.trim().parse::<u64>().ok())
    {
        return Some(Duration::from_secs(reset.saturating_sub(now_epoch).max(1)));
    }
    if status == 429 || is_secondary_limit(status, headers, body) {
        return Some(DEFAULT_SECONDARY_BACKOFF);
    }
    None
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Client {
    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(30))
            .timeout_read(Duration::from_secs(60))
            .build()
    }

    fn build(base_url: String, token: Option<String>, refresh_on_401: bool) -> Client {
        Client {
            agent: Self::agent(),
            throttle: host_throttle(&base_url),
            token,
            base_url,
            mutation_spacing: MUTATION_SPACING,
            refresh_on_401,
        }
    }

    pub fn new() -> Result<Client, GitHubError> {
        Ok(Self::build(
            BASE_URL.to_string(),
            Some(auth::resolve_token()?),
            true,
        ))
    }

    pub fn anonymous() -> Client {
        Self::build(BASE_URL.to_string(), None, false)
    }

    /// Construct a client aimed at an arbitrary base URL (e.g. a local mock
    /// server) with an explicit token. Test-only: production code goes through
    /// [`Client::new`] / [`Client::anonymous`], which always target GitHub.
    /// Mutation spacing is off so write-heavy tests don't sleep a second per
    /// write; [`Client::with_mutation_spacing`] turns it back on.
    #[cfg(test)]
    pub(crate) fn for_test(base_url: String, token: Option<String>) -> Client {
        let mut client = Self::build(base_url, token, false);
        client.mutation_spacing = Duration::ZERO;
        client
    }

    #[cfg(test)]
    pub(crate) fn with_mutation_spacing(mut self, spacing: Duration) -> Client {
        self.mutation_spacing = spacing;
        self
    }

    /// Identifies the host this client talks to, for callers keeping their
    /// own per-host caches (see `repos::settings`) -- the same keying the
    /// throttle uses, for the same reason.
    pub(crate) fn host_key(&self) -> &str {
        &self.base_url
    }

    /// Time left on this host's rate-limit backoff, if one is active.
    fn backoff_remaining(&self) -> Option<Duration> {
        let until = *self
            .throttle
            .backoff_until
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        until.and_then(|t| t.checked_duration_since(Instant::now()))
    }

    /// Extends (never shortens) this host's backoff deadline.
    fn extend_backoff(&self, wait: Duration) {
        let deadline = Instant::now() + wait;
        let mut until = self
            .throttle
            .backoff_until
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if until.is_none_or(|t| t < deadline) {
            *until = Some(deadline);
        }
    }

    pub fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, GitHubError> {
        self.request_with_accept(method, path, body, "application/vnd.github+json")
    }

    /// `request`, with the `Accept` media type chosen by the caller.
    ///
    /// GitHub uses `Accept` to select a representation, not just a format:
    /// `application/vnd.github.html+json` adds `body_html`, the comment as it
    /// is actually RENDERED. The live-GitHub verification test needs that to
    /// confirm the bridge's marker stays invisible to humans — something the
    /// default representation, which only echoes back what we wrote, cannot
    /// answer.
    pub fn request_with_accept(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
        accept: &str,
    ) -> Result<serde_json::Value, GitHubError> {
        if method != "GET" && self.token.is_none() {
            return Err(GitHubError::NoAuth(
                crate::github::auth::NO_AUTH_MSG.to_string(),
            ));
        }
        // Mutating requests go out one at a time per host, spaced apart,
        // per GitHub's guidance for staying under secondary limits. A
        // GraphQL POST counts only when its document is a mutation -- the
        // batched status lookups are queries, and spacing those out would
        // slow every sweep for nothing.
        let mutating = is_mutating(method) && (path != "/graphql" || is_graphql_mutation(&body));
        let mut mutation_slot = mutating.then(|| {
            let slot = self
                .throttle
                .last_mutation
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(wait) =
                (*slot).map(|prev| self.mutation_spacing.saturating_sub(prev.elapsed()))
                && !wait.is_zero()
            {
                std::thread::sleep(wait);
            }
            slot
        });
        let retry_body = self.refresh_on_401.then(|| body.clone()).flatten();
        let result = self.send_checked(method, path, body, accept, self.token.as_deref());
        let result = match result {
            // A cached credential can go stale mid-process (revoked, rotated
            // by `gh auth refresh`): drop it and retry once with a freshly
            // resolved one, if that is actually a different token.
            Err(GitHubError::NoAuth(_)) if self.refresh_on_401 => {
                auth::invalidate_cached_token();
                match auth::resolve_token() {
                    Ok(fresh) if Some(fresh.as_str()) != self.token.as_deref() => {
                        self.send_checked(method, path, retry_body, accept, Some(&fresh))
                    }
                    _ => result,
                }
            }
            other => other,
        };
        if let Some(slot) = mutation_slot.as_mut() {
            **slot = Some(Instant::now());
        }
        result
    }

    /// One HTTP exchange, refused up front while this host's rate-limit
    /// backoff is active, and arming that backoff when the response says to.
    fn send_checked(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
        accept: &str,
        token: Option<&str>,
    ) -> Result<serde_json::Value, GitHubError> {
        if let Some(left) = self.backoff_remaining() {
            return Err(GitHubError::RateLimited(format!(
                "GitHub rate-limit backoff active; no calls for another {}s.",
                left.as_secs().max(1)
            )));
        }
        let url = format!("{}{}", self.base_url, path);
        let mut req = self
            .agent
            .request(method, &url)
            .set("User-Agent", "agentflare")
            .set("Accept", accept)
            .set("X-GitHub-Api-Version", "2022-11-28");
        if let Some(tok) = token {
            req = req.set("Authorization", &format!("Bearer {tok}"));
        }
        // Conditional GET: the cache is keyed by representation as well as
        // URL, since `Accept` selects different bodies for the same path.
        // The cached entry is held here for the length of the exchange, so
        // a concurrent eviction can't leave a 304 with nothing to serve.
        let cache_key = (method == "GET").then(|| format!("{accept} {url}"));
        let cached: Option<CachedGet> = cache_key.as_deref().and_then(|key| {
            self.throttle
                .etags
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(key)
                .cloned()
        });
        if let Some(c) = &cached {
            req = req.set("If-None-Match", &c.etag);
        }
        let result = match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        };
        match result {
            Ok(resp) if resp.status() == 304 => {
                // Unchanged since the validator we sent: GitHub charged no
                // rate-limit budget for this, and the body we kept is current.
                let Some(c) = cached else {
                    return Err(GitHubError::Transport(
                        "GitHub answered 304 to a request that sent no validator".to_string(),
                    ));
                };
                serde_json::from_str(&c.body).map_err(|e| GitHubError::Parse(e.to_string()))
            }
            Ok(resp) => {
                let etag = resp.header("etag").map(str::to_string);
                let text = resp
                    .into_string()
                    .map_err(|e| GitHubError::Transport(e.to_string()))?;
                if let Some(key) = cache_key.as_deref() {
                    let mut etags = self
                        .throttle
                        .etags
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    match etag {
                        Some(etag) => etags.insert(key, &etag, &text),
                        // No validator on this response: whatever we held
                        // for the URL is stale now.
                        None => etags.remove(key),
                    }
                }
                if text.trim().is_empty() {
                    return Ok(serde_json::Value::Null);
                }
                serde_json::from_str(&text).map_err(|e| GitHubError::Parse(e.to_string()))
            }
            Err(ureq::Error::Status(code, resp)) => {
                let headers = RateHeaders {
                    remaining: resp.header("x-ratelimit-remaining").map(str::to_string),
                    reset: resp.header("x-ratelimit-reset").map(str::to_string),
                    retry_after: resp.header("retry-after").map(str::to_string),
                };
                let body = resp.into_string().unwrap_or_default();
                if let Some(wait) = backoff_for(code, &headers, &body, now_epoch()) {
                    self.extend_backoff(wait);
                }
                Err(map_status(code, &headers, body))
            }
            Err(e) => Err(GitHubError::Transport(e.to_string())),
        }
    }

    /// GET every page of a list endpoint, walking `?page=N&per_page=100` until a
    /// page comes back shorter than a full page. `extract` pulls the item array
    /// out of each response — identity for bare-array endpoints, or the envelope
    /// key (e.g. `workflow_runs`) for wrapped ones. Returns the concatenated
    /// items as a JSON array so callers deserialize once.
    pub fn get_paginated(
        &self,
        base_path: &str,
        extract: impl Fn(&serde_json::Value) -> Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, GitHubError> {
        let sep = if base_path.contains('?') { '&' } else { '?' };
        let mut all: Vec<serde_json::Value> = Vec::new();
        let mut page = 1usize;
        loop {
            let path = format!("{base_path}{sep}per_page={PER_PAGE}&page={page}");
            let resp = self.request("GET", &path, None)?;
            let items = extract(&resp);
            let full = items.len() == PER_PAGE;
            all.extend(items);
            if !full {
                break;
            }
            page += 1;
        }
        Ok(serde_json::Value::Array(all))
    }
}

/// Extractor for bare-array list endpoints (pulls, issues, releases).
pub fn as_array(v: &serde_json::Value) -> Vec<serde_json::Value> {
    v.as_array().cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remaining(v: &str) -> RateHeaders {
        RateHeaders {
            remaining: Some(v.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn map_status_covers_the_table() {
        let none = RateHeaders::default();
        assert!(matches!(
            map_status(401, &none, String::new()),
            GitHubError::NoAuth(_)
        ));
        assert!(matches!(
            map_status(403, &remaining("0"), String::new()),
            GitHubError::RateLimited(_)
        ));
        assert!(matches!(
            map_status(403, &remaining("42"), String::new()),
            GitHubError::Forbidden(_)
        ));
        assert!(matches!(
            map_status(404, &none, String::new()),
            GitHubError::NotFound
        ));
        assert!(matches!(
            map_status(500, &none, "boom".into()),
            GitHubError::Http { status: 500, .. }
        ));
    }

    #[test]
    fn map_status_detects_secondary_limits_despite_remaining_budget() {
        let body = r#"{"message":"You have exceeded a secondary rate limit."}"#.to_string();
        assert!(matches!(
            map_status(403, &remaining("4000"), body),
            GitHubError::RateLimited(_)
        ));
        let abuse = r#"{"message":"abuse detection mechanism"}"#.to_string();
        assert!(matches!(
            map_status(403, &remaining("4000"), abuse),
            GitHubError::RateLimited(_)
        ));
        let retry = RateHeaders {
            remaining: Some("4000".into()),
            retry_after: Some("30".into()),
            ..Default::default()
        };
        assert!(matches!(
            map_status(403, &retry, String::new()),
            GitHubError::RateLimited(_)
        ));
    }

    #[test]
    fn backoff_for_prefers_retry_after_then_reset_then_default() {
        let retry = RateHeaders {
            retry_after: Some("30".into()),
            remaining: Some("0".into()),
            reset: Some("2000".into()),
        };
        assert_eq!(
            backoff_for(403, &retry, "", 1000),
            Some(Duration::from_secs(30))
        );
        let reset = RateHeaders {
            remaining: Some("0".into()),
            reset: Some("1090".into()),
            ..Default::default()
        };
        assert_eq!(
            backoff_for(403, &reset, "", 1000),
            Some(Duration::from_secs(90))
        );
        assert_eq!(
            backoff_for(429, &RateHeaders::default(), "", 1000),
            Some(DEFAULT_SECONDARY_BACKOFF)
        );
        assert_eq!(
            backoff_for(403, &remaining("5"), "secondary rate limit", 1000),
            Some(DEFAULT_SECONDARY_BACKOFF)
        );
        // A plain permission 403 and non-limit statuses arm no backoff.
        assert_eq!(backoff_for(403, &remaining("5"), "", 1000), None);
        assert_eq!(backoff_for(500, &RateHeaders::default(), "", 1000), None);
    }

    #[test]
    fn is_graphql_mutation_tells_mutations_from_queries() {
        let q = |s: &str| Some(serde_json::json!({ "query": s }));
        assert!(is_graphql_mutation(&q(
            "mutation($input: X!) { enablePullRequestAutoMerge(input: $input) { x } }"
        )));
        assert!(is_graphql_mutation(&q("  mutation { x }")));
        assert!(!is_graphql_mutation(&q("query($owner:String!){ x }")));
        assert!(!is_graphql_mutation(&None));
    }

    #[test]
    fn as_array_unwraps_or_defaults_empty() {
        assert_eq!(as_array(&serde_json::json!([1, 2, 3])).len(), 3);
        assert!(as_array(&serde_json::json!({})).is_empty());
        assert!(as_array(&serde_json::Value::Null).is_empty());
    }

    #[test]
    fn map_status_429_is_rate_limited() {
        assert!(matches!(
            map_status(429, &RateHeaders::default(), String::new()),
            GitHubError::RateLimited(_)
        ));
    }

    use crate::github::test_support::{MockResponse, MockServer};

    #[test]
    fn request_refuses_writes_without_a_token_before_any_network_call() {
        // No server needed: the guard short-circuits before a request is made.
        let client = Client::for_test("http://127.0.0.1:1".to_string(), None);
        let err = client.request("POST", "/x", None).unwrap_err();
        assert!(matches!(err, GitHubError::NoAuth(_)));
    }

    #[test]
    fn request_get_parses_json_and_sends_expected_headers() {
        let server = MockServer::start(vec![MockResponse::json(200, r#"{"ok":true}"#)]);
        let client = server.client(None);
        let value = client.request("GET", "/probe", None).unwrap();
        assert_eq!(value["ok"], true);

        let reqs = server.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "GET");
        assert_eq!(reqs[0].path, "/probe");
    }

    #[test]
    fn request_sends_json_body_on_writes() {
        let server = MockServer::start(vec![MockResponse::json(201, r#"{"created":1}"#)]);
        let client = server.client(Some("tok"));
        let body = serde_json::json!({ "name": "value" });
        let value = client.request("POST", "/thing", Some(body)).unwrap();
        assert_eq!(value["created"], 1);

        let reqs = server.requests();
        assert_eq!(reqs[0].method, "POST");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["name"], "value");
    }

    #[test]
    fn request_empty_body_becomes_json_null() {
        let server = MockServer::start(vec![MockResponse::json(204, "")]);
        let client = server.client(Some("tok"));
        let value = client
            .request("POST", "/empty", Some(serde_json::json!({})))
            .unwrap();
        assert_eq!(value, serde_json::Value::Null);
        let _ = server.requests();
    }

    #[test]
    fn request_maps_error_status_from_the_wire() {
        let server = MockServer::start(vec![
            MockResponse::json(403, r#"{"message":"limited"}"#)
                .with_header("x-ratelimit-remaining", "0"),
        ]);
        let client = server.client(Some("tok"));
        let err = client.request("GET", "/limited", None).unwrap_err();
        assert!(matches!(err, GitHubError::RateLimited(_)));
        let _ = server.requests();
    }

    #[test]
    fn request_reports_parse_error_on_malformed_json() {
        let server = MockServer::start(vec![MockResponse::json(200, "not json")]);
        let client = server.client(None);
        let err = client.request("GET", "/bad", None).unwrap_err();
        assert!(matches!(err, GitHubError::Parse(_)));
        let _ = server.requests();
    }

    #[test]
    fn a_secondary_limit_arms_a_backoff_that_refuses_further_calls_without_touching_the_network() {
        let server = MockServer::start(vec![
            MockResponse::json(
                403,
                r#"{"message":"You have exceeded a secondary rate limit"}"#,
            )
            .with_header("retry-after", "60")
            .with_header("x-ratelimit-remaining", "4000"),
        ]);
        let client = server.client(Some("tok"));
        let err = client.request("GET", "/first", None).unwrap_err();
        assert!(matches!(err, GitHubError::RateLimited(_)));
        // A second client for the same host shares the deadline.
        let other = client.clone();
        let err = other.request("GET", "/second", None).unwrap_err();
        assert!(matches!(err, GitHubError::RateLimited(_)));
        let reqs = server.requests();
        assert_eq!(
            reqs.len(),
            1,
            "the backoff must stop the second call locally"
        );
    }

    #[test]
    fn a_repeated_get_sends_if_none_match_and_serves_the_cached_body_on_304() {
        let server = MockServer::start(vec![
            MockResponse::json(200, r#"{"a":1}"#).with_header("ETag", "W/\"v1\""),
            MockResponse::json(304, ""),
            MockResponse::json(200, r#"{"a":2}"#).with_header("ETag", "W/\"v2\""),
            MockResponse::json(304, ""),
        ]);
        let client = server.client(Some("tok"));
        assert_eq!(client.request("GET", "/thing", None).unwrap()["a"], 1);
        // Unchanged: the server says 304 and the first body is served again.
        assert_eq!(client.request("GET", "/thing", None).unwrap()["a"], 1);
        // Changed: a fresh body replaces the cached one and its validator.
        assert_eq!(client.request("GET", "/thing", None).unwrap()["a"], 2);
        assert_eq!(client.request("GET", "/thing", None).unwrap()["a"], 2);

        let reqs = server.requests();
        assert_eq!(reqs.len(), 4);
        assert_eq!(reqs[0].header("if-none-match"), None);
        assert_eq!(reqs[1].header("if-none-match"), Some("W/\"v1\""));
        assert_eq!(reqs[2].header("if-none-match"), Some("W/\"v1\""));
        assert_eq!(reqs[3].header("if-none-match"), Some("W/\"v2\""));
    }

    #[test]
    fn the_etag_cache_is_keyed_by_representation_and_only_used_for_gets() {
        let server = MockServer::start(vec![
            MockResponse::json(200, r#"{"a":1}"#).with_header("ETag", "\"v1\""),
            MockResponse::json(200, r#"{"a":1,"body_html":"x"}"#).with_header("ETag", "\"h1\""),
            MockResponse::json(201, r#"{"a":1}"#).with_header("ETag", "\"p1\""),
            MockResponse::json(304, ""),
        ]);
        let client = server.client(Some("tok"));
        client.request("GET", "/thing", None).unwrap();
        client
            .request_with_accept("GET", "/thing", None, "application/vnd.github.html+json")
            .unwrap();
        client
            .request("POST", "/thing", Some(serde_json::json!({})))
            .unwrap();
        let again = client.request("GET", "/thing", None).unwrap();
        assert_eq!(again["a"], 1);
        assert!(
            again.get("body_html").is_none(),
            "served the json+json body"
        );

        let reqs = server.requests();
        assert_eq!(
            reqs[1].header("if-none-match"),
            None,
            "other Accept, other key"
        );
        assert_eq!(
            reqs[2].header("if-none-match"),
            None,
            "writes are never conditional"
        );
        assert_eq!(reqs[3].header("if-none-match"), Some("\"v1\""));
    }

    #[test]
    fn a_response_without_an_etag_evicts_the_stale_entry() {
        let server = MockServer::start(vec![
            MockResponse::json(200, r#"{"a":1}"#).with_header("ETag", "\"v1\""),
            MockResponse::json(200, r#"{"a":2}"#),
            MockResponse::json(200, r#"{"a":3}"#),
        ]);
        let client = server.client(Some("tok"));
        client.request("GET", "/thing", None).unwrap();
        assert_eq!(client.request("GET", "/thing", None).unwrap()["a"], 2);
        assert_eq!(client.request("GET", "/thing", None).unwrap()["a"], 3);
        let reqs = server.requests();
        assert_eq!(reqs[1].header("if-none-match"), Some("\"v1\""));
        assert_eq!(
            reqs[2].header("if-none-match"),
            None,
            "the validator went with the body it validated"
        );
    }

    #[test]
    fn a_rate_limit_backoff_still_refuses_a_get_that_has_a_cached_copy() {
        // A cached body is only known current after the server confirms it
        // with a 304, so a backoff must still keep the call off the network
        // rather than serve the stale copy as if it were fresh.
        let server = MockServer::start(vec![
            MockResponse::json(200, r#"{"a":1}"#).with_header("ETag", "\"v1\""),
            MockResponse::json(403, r#"{"message":"secondary rate limit"}"#)
                .with_header("retry-after", "60"),
        ]);
        let client = server.client(Some("tok"));
        client.request("GET", "/thing", None).unwrap();
        assert!(matches!(
            client.request("GET", "/other", None).unwrap_err(),
            GitHubError::RateLimited(_)
        ));
        assert!(matches!(
            client.request("GET", "/thing", None).unwrap_err(),
            GitHubError::RateLimited(_)
        ));
        assert_eq!(
            server.requests().len(),
            2,
            "the backoff stopped the third call locally"
        );
    }

    #[test]
    fn mutating_requests_are_spaced_apart() {
        let server = MockServer::start(vec![
            MockResponse::json(201, "{}"),
            MockResponse::json(201, "{}"),
            MockResponse::json(200, "{}"),
        ]);
        let client = server
            .client(Some("tok"))
            .with_mutation_spacing(Duration::from_millis(300));
        let started = Instant::now();
        client
            .request("POST", "/a", Some(serde_json::json!({})))
            .unwrap();
        client
            .request("POST", "/b", Some(serde_json::json!({})))
            .unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(300),
            "the second write must wait out the spacing"
        );
        // Reads are never delayed by the write spacing.
        let read_started = Instant::now();
        client.request("GET", "/c", None).unwrap();
        assert!(read_started.elapsed() < Duration::from_millis(300));
        assert_eq!(server.requests().len(), 3);
    }
}
