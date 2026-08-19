use serde_json::Value;
use std::path::{Path, PathBuf};

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

/// Reads `providers.json` and returns the stored access token for the Cline
/// CLI login that maps to `provider_id`. Returns `None` when the login is
/// missing or its token is already past `expiresAt` (epoch ms).
pub fn cli_access_token(provider_id: &str) -> Option<String> {
    let data_dir = cline_data_dir()?;
    let key = cli_provider_key(provider_id)?;
    read_access_token(&data_dir.join("settings").join("providers.json"), key)
}

fn read_access_token(path: &Path, provider_key: &str) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let json: Value = serde_json::from_str(&raw).ok()?;
    let pointer = format!("/providers/{provider_key}/settings/auth");
    let auth = json.pointer(&pointer)?;
    let token = auth.get("accessToken")?.as_str()?.to_string();
    if let Some(exp) = auth.get("expiresAt").and_then(Value::as_i64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis() as i64;
        if exp < now {
            return None;
        }
    }
    Some(token)
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
                            "expiresAt": 1787160890000
                        }
                    },
                    "tokenSource": "oauth"
                }
            }
        }"#;
        let p = fixture(json);
        assert_eq!(
            read_access_token(&p, "cline-pass").as_deref(),
            Some("tok-abc")
        );
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
        assert_eq!(read_access_token(&p, "cline"), None);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn returns_none_when_login_missing() {
        let json = r#"{"providers": {}}"#;
        let p = fixture(json);
        assert_eq!(read_access_token(&p, "cline"), None);
        let _ = std::fs::remove_file(&p);
    }
}
