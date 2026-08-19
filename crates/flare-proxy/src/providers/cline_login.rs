use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Cline's OAuth refresh endpoint (verified against the cline/cline SDK,
/// `sdk/packages/core/src/auth/cline.ts` → `DEFAULT_AUTH_ENDPOINTS.refresh`).
const REFRESH_URL: &str = "https://api.cline.bot/api/v1/auth/refresh";

/// Refresh the token before it is within this window of expiry, mirroring the
/// SDK's `DEFAULT_REFRESH_BUFFER_MS`.
const REFRESH_BUFFER_MS: i64 = 5 * 60 * 1000;

/// Conservative lifetime for a token we refreshed ourselves: we cannot parse
/// the ISO `expiresAt` from the refresh response without chrono, and the
/// stored tokens live ~1h, so 50min is safe.
const REFRESHED_TTL_MS: i64 = 50 * 60 * 1000;

/// In-process cache of self-refreshed tokens (keyed by cli provider key), so
/// a proxy that outlives the CLI's persisted token does not re-refresh on
/// every request. We never write back to the CLI's `providers.json`.
static REFRESH_CACHE: OnceLock<Mutex<HashMap<String, CachedToken>>> = OnceLock::new();

#[derive(Clone)]
struct CachedToken {
    token: String,
    expires_ms: i64,
}

/// Maps a flare-proxy provider id to the key used inside the Cline CLI's
/// `providers.json` (`providers.<key>.settings.auth`). Only cline/clinepass
/// have a CLI login to fall back on.
fn cli_provider_key(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "cline" => Some("cline"),
        "clinepass" => Some("cline-pass"),
        _ => None,
    }
}

/// Cline CLI data directory: `~/.cline/data`. No `CLINE_HOME` override is
/// honored today — the CLI's default location is the only one we watch.
fn cline_data_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cline").join("data"))
}

fn providers_json_path() -> Option<PathBuf> {
    cline_data_dir().map(|d| d.join("settings").join("providers.json"))
}

#[derive(Debug, Default)]
struct AuthBlock {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_ms: Option<i64>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn read_auth_block(path: &Path, provider_key: &str) -> Option<AuthBlock> {
    let raw = std::fs::read_to_string(path).ok()?;
    let json: Value = serde_json::from_str(&raw).ok()?;
    let auth = json.pointer(&format!("/providers/{provider_key}/settings/auth"))?;
    Some(AuthBlock {
        access_token: auth
            .get("accessToken")
            .and_then(Value::as_str)
            .map(String::from),
        refresh_token: auth
            .get("refreshToken")
            .and_then(Value::as_str)
            .map(String::from),
        expires_ms: auth.get("expiresAt").and_then(Value::as_i64),
    })
}

/// True while the token is more than `REFRESH_BUFFER_MS` from expiry (or has
/// no expiry recorded).
fn token_still_valid(expires_ms: Option<i64>) -> bool {
    match expires_ms {
        None => true,
        Some(exp) => exp.saturating_sub(now_ms()) > REFRESH_BUFFER_MS,
    }
}

/// Returns the access token for the Cline CLI login that maps to
/// `provider_id`: the stored token when still valid, otherwise a token we
/// refresh ourselves from the stored refresh token (cached in-process).
pub async fn cli_credential(client: &reqwest::Client, provider_id: &str) -> Option<String> {
    let provider_key = cli_provider_key(provider_id)?;
    let path = providers_json_path()?;
    let auth = read_auth_block(&path, provider_key)?;

    let access = auth.access_token?;
    if token_still_valid(auth.expires_ms) {
        return Some(access);
    }

    let refresh_token = auth.refresh_token?;
    if let Some(cached) = cached(provider_key) {
        if cached.expires_ms.saturating_sub(now_ms()) > REFRESH_BUFFER_MS {
            return Some(cached.token);
        }
    }

    let body = json!({ "refreshToken": refresh_token, "grantType": "refresh_token" });
    let resp = client
        .post(REFRESH_URL)
        .header("Content-Type", "application/json")
        .header("HTTP-Referer", "https://cline.bot")
        .header("X-Title", "Cline")
        .json(&body)
        .send()
        .await
        .ok()?;
    let json: Value = resp.json().await.ok()?;
    if json.get("success").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let token = json
        .pointer("/data/accessToken")
        .and_then(Value::as_str)?
        .to_string();
    let cached_token = CachedToken {
        token: token.clone(),
        expires_ms: now_ms() + REFRESHED_TTL_MS,
    };
    cache(provider_key, cached_token);
    Some(token)
}

fn cache(provider_key: &str, value: CachedToken) {
    let mut map = REFRESH_CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    map.insert(provider_key.to_string(), value);
}

fn cached(provider_key: &str) -> Option<CachedToken> {
    REFRESH_CACHE
        .get()?
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(provider_key)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn fixture(json: &str) -> std::path::PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "flare-proxy-cline-login-{}-{n}.json",
            std::process::id()
        ));
        std::fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn maps_provider_ids_to_cli_keys() {
        assert_eq!(cli_provider_key("cline"), Some("cline"));
        assert_eq!(cli_provider_key("clinepass"), Some("cline-pass"));
        assert_eq!(cli_provider_key("anthropic"), None);
    }

    #[test]
    fn reads_valid_login_token() {
        let json = r#"{
            "providers": {
                "cline-pass": {
                    "settings": {
                        "provider": "cline-pass",
                        "auth": {
                            "accessToken": "tok-abc",
                            "refreshToken": "rt-abc",
                            "expiresAt": 4102444800000
                        }
                    },
                    "tokenSource": "oauth"
                }
            }
        }"#;
        let p = fixture(json);
        let auth = read_auth_block(&p, "cline-pass").unwrap();
        assert_eq!(auth.access_token.as_deref(), Some("tok-abc"));
        assert_eq!(auth.refresh_token.as_deref(), Some("rt-abc"));
        assert!(token_still_valid(auth.expires_ms));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rejects_expired_login_token() {
        let json = r#"{
            "providers": {
                "cline": {
                    "settings": {
                        "auth": { "accessToken": "tok-expired", "expiresAt": 1 }
                    }
                }
            }
        }"#;
        let p = fixture(json);
        let auth = read_auth_block(&p, "cline").unwrap();
        assert!(!token_still_valid(auth.expires_ms));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn treats_token_within_refresh_buffer_as_expired() {
        let soon = now_ms() + REFRESH_BUFFER_MS / 2;
        assert!(!token_still_valid(Some(soon)));
        let ok = now_ms() + 2 * REFRESH_BUFFER_MS;
        assert!(token_still_valid(Some(ok)));
    }

    #[test]
    fn returns_none_when_login_missing() {
        let json = r#"{"providers": {}}"#;
        let p = fixture(json);
        assert!(read_auth_block(&p, "cline").is_none());
        let _ = std::fs::remove_file(&p);
    }
}
