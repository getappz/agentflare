mod forward;
pub mod heuristic;
pub mod providers;
pub mod shape_xlat;
pub mod shortcircuit;
pub mod think;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
pub use providers::ProviderConfig;
use std::time::Duration;

pub fn router() -> Router {
    router_with_config(ProviderConfig::from_env())
}

/// Same as [`router`], but with an explicit [`ProviderConfig`] instead of
/// one read from `MODEL`/`MODEL_OPUS`/`MODEL_SONNET`/`MODEL_HAIKU` env vars.
///
/// For embedders that build their own routing table — e.g. a multi-model
/// consensus engine mapping several participant model handles to explicit
/// `(provider, upstream_model)` pairs, which doesn't fit `from_env()`'s
/// single-active-model-per-role shape. Build each provider's `ProviderEntry`
/// via [`providers::provider_entry`] and add one [`providers::ModelRoute`]
/// per handle.
pub fn router_with_config(config: ProviderConfig) -> Router {
    // Best-effort, short-timeout refresh of the provider registry before
    // the first ProviderConfig::from_env() read — see
    // providers::ensure_fresh_registry for what "on demand" means here.
    providers::ensure_fresh_registry();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap_or_default();
    Router::new()
        .route("/proxy/v1/messages", post(v1_messages_handler))
        .with_state(AppState {
            config,
            sc_config: shortcircuit::ShortCircuitConfig::from_env(),
            client,
        })
}

/// When `AGENTFLARE_PROXY_TOKEN` is set, requests must carry a matching
/// `x-agentflare-proxy-token` header. This route forwards to paid/free
/// upstream APIs using server-held credentials and is mounted on the
/// dashboard server, which can be bound off-localhost — without this gate
/// anyone reachable on the network could spend the operator's provider quota.
async fn v1_messages_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Json(body): axum::extract::Json<serde_json::Value>,
) -> Response {
    if let Ok(expected) = std::env::var("AGENTFLARE_PROXY_TOKEN") {
        if expected.is_empty() {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "AGENTFLARE_PROXY_TOKEN is set but empty",
            )
                .into_response();
        }
        let provided = headers
            .get("x-agentflare-proxy-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if provided != expected {
            return (StatusCode::UNAUTHORIZED, "invalid or missing proxy token").into_response();
        }
    }
    match shortcircuit::try_short_circuit(&body, &state.sc_config) {
        shortcircuit::ShortCircuitOutcome::Match(message) => {
            // Internal CLI bookkeeping calls (quota probe, prefix/title/
            // suggestion/filepath detection) are sent non-streaming and
            // expect a plain Messages JSON body, unlike every other request
            // this proxy handles.
            return axum::Json(message).into_response();
        }
        shortcircuit::ShortCircuitOutcome::NoMatch => {}
    }
    forward::proxy_request(body, &state.config, &state.client).await
}

#[derive(Clone)]
struct AppState {
    config: ProviderConfig,
    sc_config: shortcircuit::ShortCircuitConfig,
    client: reqwest::Client,
}

/// Serializes tests that mutate process-global env vars (`MODEL`,
/// `MODEL_OPUS`, ...) — `cargo test` runs tests in parallel by default, and
/// unsynchronized `env::set_var` calls across tests race.
#[cfg(test)]
pub(crate) struct TestEnvGuard {
    previous_home: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for TestEnvGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.previous_home {
            std::env::set_var("AGENTFLARE_HOME_OVERRIDE", value);
        } else {
            std::env::remove_var("AGENTFLARE_HOME_OVERRIDE");
        }
    }
}

#[cfg(test)]
pub(crate) fn test_env_lock() -> TestEnvGuard {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    TestEnvGuard {
        previous_home: std::env::var_os("AGENTFLARE_HOME_OVERRIDE"),
        _lock: lock,
    }
}

#[test]
fn home_override_survives_test_env_scope() {
    const CHILD: &str = "FLARE_PROXY_HOME_RESTORE_CHILD";
    const HOME: &str = "AGENTFLARE_HOME_OVERRIDE";
    if std::env::var_os(CHILD).is_none() {
        for inherited in [true, false] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args(["--exact", "home_override_survives_test_env_scope"])
                .env(CHILD, "1");
            if inherited {
                child.env(HOME, std::env::temp_dir().join("proxy-outer-test-home"));
            } else {
                child.env_remove(HOME);
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "inherited={inherited}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }
    let before = std::env::var_os(HOME);
    for unwind in [false, true] {
        let result = std::panic::catch_unwind(|| {
            let _guard = test_env_lock();
            std::env::set_var(HOME, std::env::temp_dir().join("proxy-scoped-test-home"));
            assert!(!unwind, "exercise panic cleanup");
            std::env::remove_var(HOME);
        });
        assert_eq!(result.is_err(), unwind);
        assert_eq!(std::env::var_os(HOME), before);
    }
}
