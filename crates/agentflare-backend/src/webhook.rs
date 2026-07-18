use hmac::{Hmac, Mac};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::io::Read;
use std::sync::OnceLock;

use crate::error::{Error, Result};

/// Delivery response bodies are logged to SQLite (`webhook_logs.response_body`) —
/// cap what we read/store so a huge or malicious endpoint response can't exhaust
/// memory or bloat the database.
const MAX_LOGGED_BODY_BYTES: usize = 8 * 1024;

/// Shared HTTP client with redirects disabled: an SSRF'd webhook target could
/// otherwise 3xx-redirect delivery to an internal address after the URL itself
/// passed `validate_webhook_url`. A blocked redirect is returned as an
/// ordinary (unfollowed) 3xx response, which `deliver()` logs like any other.
fn http_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| ureq::AgentBuilder::new().redirects(0).build())
}

fn capped_body(resp: ureq::Response) -> String {
    let mut buf = Vec::with_capacity(MAX_LOGGED_BODY_BYTES + 1);
    let read = resp
        .into_reader()
        .take(MAX_LOGGED_BODY_BYTES as u64 + 1)
        .read_to_end(&mut buf);
    if read.is_err() {
        return String::new();
    }
    let truncated = buf.len() > MAX_LOGGED_BODY_BYTES;
    buf.truncate(MAX_LOGGED_BODY_BYTES);
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        s.push_str("...[truncated]");
    }
    s
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Webhook {
    pub id: String,
    pub workspace_id: String,
    pub url: String,
    pub is_active: bool,
    #[serde(skip_serializing)]
    pub secret_key: String,
    pub on_item: bool,
    pub on_state: bool,
    pub on_project: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWebhook {
    pub workspace_id: String,
    pub url: String,
    pub secret_key: String,
    pub on_item: Option<bool>,
    pub on_state: Option<bool>,
    pub on_project: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateWebhook {
    pub url: Option<String>,
    pub is_active: Option<bool>,
    pub on_item: Option<bool>,
    pub on_state: Option<bool>,
    pub on_project: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookLog {
    pub id: String,
    pub workspace_id: String,
    pub webhook_id: String,
    pub event_type: Option<String>,
    pub request_method: Option<String>,
    pub request_headers: Option<String>,
    pub request_body: Option<String>,
    pub response_status: Option<String>,
    pub response_headers: Option<String>,
    pub response_body: Option<String>,
    pub retry_count: i64,
    pub created_at: i64,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn row_to_webhook(row: &rusqlite::Row) -> rusqlite::Result<Webhook> {
    Ok(Webhook {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        url: row.get(2)?,
        is_active: row.get::<_, i64>(3)? != 0,
        secret_key: row.get(4)?,
        on_item: row.get::<_, i64>(5)? != 0,
        on_state: row.get::<_, i64>(6)? != 0,
        on_project: row.get::<_, i64>(7)? != 0,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        deleted_at: row.get(10)?,
    })
}

fn row_to_webhook_log(row: &rusqlite::Row) -> rusqlite::Result<WebhookLog> {
    Ok(WebhookLog {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        webhook_id: row.get(2)?,
        event_type: row.get(3)?,
        request_method: row.get(4)?,
        request_headers: row.get(5)?,
        request_body: row.get(6)?,
        response_status: row.get(7)?,
        response_headers: row.get(8)?,
        response_body: row.get(9)?,
        retry_count: row.get(10)?,
        created_at: row.get(11)?,
    })
}

/// Blocks loopback, unspecified, multicast, and private/link-local ranges
/// (RFC1918, 169.254.0.0/16, IPv6 link-local fe80::/10, IPv6 ULA fc00::/7) —
/// the ranges an SSRF'd webhook could use to reach internal services.
fn is_blocked_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
        }
        std::net::IpAddr::V6(v6) => {
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg0 & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (seg0 & 0xfe00) == 0xfc00 // unique local fc00::/7
        }
    }
}

/// A domain can resolve to a private/link-local address even when its literal
/// spelling looks external (DNS rebinding, or a hostname the operator points
/// at `169.254.169.254`), which the IP-literal checks above can't catch.
/// Resolve the host and reject if *any* returned address is blocked. A
/// resolution failure is treated as "not provably internal" and allowed
/// through — a delivery to an unresolvable host fails on its own without
/// blocking legitimate hooks on transient DNS errors.
fn domain_resolves_to_blocked_ip(domain: &str, port: u16) -> bool {
    use std::net::ToSocketAddrs;
    match (domain, port).to_socket_addrs() {
        Ok(addrs) => addrs.into_iter().any(|sa| is_blocked_ip(sa.ip())),
        Err(_) => false,
    }
}

fn validate_webhook_url(raw: &str) -> Result<()> {
    let parsed =
        url::Url::parse(raw).map_err(|_| Error::InvalidTransition("invalid webhook URL".into()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::InvalidTransition(
            "webhook URL must use http or https scheme".into(),
        ));
    }
    let default_port = if parsed.scheme() == "https" { 443 } else { 80 };
    let port = parsed.port().unwrap_or(default_port);
    let blocked = match parsed.host() {
        Some(url::Host::Ipv4(v4)) => is_blocked_ip(std::net::IpAddr::V4(v4)),
        Some(url::Host::Ipv6(v6)) => is_blocked_ip(std::net::IpAddr::V6(v6)),
        Some(url::Host::Domain(d)) => {
            d.eq_ignore_ascii_case("localhost") || domain_resolves_to_blocked_ip(d, port)
        }
        None => true,
    };
    if blocked {
        return Err(Error::InvalidTransition(
            "webhook URL must not target localhost or a private/link-local address".into(),
        ));
    }
    Ok(())
}

pub fn create(conn: &Connection, input: CreateWebhook) -> Result<Webhook> {
    validate_webhook_url(&input.url)?;
    let id = uuid::Uuid::now_v7().to_string();
    let ts = now();
    conn.execute(
        "INSERT INTO webhooks (id, workspace_id, url, is_active, secret_key, on_item, on_state, on_project, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            input.workspace_id,
            input.url,
            true,
            input.secret_key,
            input.on_item.unwrap_or(false),
            input.on_state.unwrap_or(false),
            input.on_project.unwrap_or(false),
            ts,
            ts,
        ],
    )?;
    get(conn, &id)
}

pub fn get(conn: &Connection, id: &str) -> Result<Webhook> {
    conn.query_row(
        "SELECT id, workspace_id, url, is_active, secret_key, on_item, on_state, on_project, created_at, updated_at, deleted_at
         FROM webhooks WHERE id = ?1 AND deleted_at IS NULL",
        params![id],
        row_to_webhook,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Error::NotFound(id.to_string()),
        other => other.into(),
    })
}

pub fn list_by_workspace(conn: &Connection, workspace_id: &str) -> Result<Vec<Webhook>> {
    let mut stmt = conn.prepare(
        "SELECT id, workspace_id, url, is_active, secret_key, on_item, on_state, on_project, created_at, updated_at, deleted_at
         FROM webhooks WHERE workspace_id = ?1 AND deleted_at IS NULL ORDER BY created_at",
    )?;
    let rows = stmt.query_map(params![workspace_id], row_to_webhook)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn list_active_matching(
    conn: &Connection,
    workspace_id: &str,
    event_type: &str,
) -> Result<Vec<Webhook>> {
    let column = match event_type {
        "item" => "on_item",
        "state" => "on_state",
        "project" => "on_project",
        _ => return Ok(Vec::new()),
    };
    let sql = format!(
        "SELECT id, workspace_id, url, is_active, secret_key, on_item, on_state, on_project, created_at, updated_at, deleted_at
         FROM webhooks WHERE workspace_id = ?1 AND is_active = 1 AND deleted_at IS NULL AND {} = 1 ORDER BY created_at",
        column
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![workspace_id], row_to_webhook)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Delivery log entries for a workspace, most recent first — the audit
/// trail the dashboard's `/api/webhooks` view reads.
pub fn list_logs_by_workspace(conn: &Connection, workspace_id: &str) -> Result<Vec<WebhookLog>> {
    let mut stmt = conn.prepare(
        "SELECT id, workspace_id, webhook_id, event_type, request_method, request_headers, request_body, response_status, response_headers, response_body, retry_count, created_at
         FROM webhook_logs WHERE workspace_id = ?1 ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map(params![workspace_id], row_to_webhook_log)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn update(conn: &Connection, id: &str, input: UpdateWebhook) -> Result<Webhook> {
    if let Some(ref url) = input.url {
        validate_webhook_url(url)?;
    }
    let ts = now();
    let mut sets = vec!["updated_at = ?2".to_string()];
    let mut param_idx = 3;
    if input.url.is_some() {
        sets.push(format!("url = ?{param_idx}"));
        param_idx += 1;
    }
    if input.is_active.is_some() {
        sets.push(format!("is_active = ?{param_idx}"));
        param_idx += 1;
    }
    if input.on_item.is_some() {
        sets.push(format!("on_item = ?{param_idx}"));
        param_idx += 1;
    }
    if input.on_state.is_some() {
        sets.push(format!("on_state = ?{param_idx}"));
        param_idx += 1;
    }
    if input.on_project.is_some() {
        sets.push(format!("on_project = ?{param_idx}"));
    }
    let sql = format!(
        "UPDATE webhooks SET {} WHERE id = ?1 AND deleted_at IS NULL",
        sets.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(id.to_string()));
    param_values.push(Box::new(ts));
    if let Some(ref url) = input.url {
        param_values.push(Box::new(url.clone()));
    }
    if let Some(active) = input.is_active {
        param_values.push(Box::new(active as i64));
    }
    if let Some(on) = input.on_item {
        param_values.push(Box::new(on as i64));
    }
    if let Some(on) = input.on_state {
        param_values.push(Box::new(on as i64));
    }
    if let Some(on) = input.on_project {
        param_values.push(Box::new(on as i64));
    }
    let changed = stmt.execute(rusqlite::params_from_iter(param_values.iter()))?;
    if changed == 0 {
        return Err(Error::NotFound(id.to_string()));
    }
    get(conn, id)
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    let ts = now();
    let changed = conn.execute(
        "UPDATE webhooks SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
        params![ts, id],
    )?;
    if changed == 0 {
        return Err(Error::NotFound(id.to_string()));
    }
    Ok(())
}

fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

fn log_delivery(
    conn: &Connection,
    webhook: &Webhook,
    event_type: &str,
    request_body: &[u8],
    status: &str,
    resp_body: &str,
) -> Result<()> {
    let id = uuid::Uuid::now_v7().to_string();
    let ts = now();
    conn.execute(
        "INSERT INTO webhook_logs (id, workspace_id, webhook_id, event_type, request_method, request_body, response_status, response_body, retry_count, created_at)
         VALUES (?1, ?2, ?3, ?4, 'POST', ?5, ?6, ?7, 0, ?8)",
        params![
            id,
            webhook.workspace_id,
            webhook.id,
            event_type,
            String::from_utf8_lossy(request_body).to_string(),
            status,
            resp_body,
            ts,
        ],
    )?;
    Ok(())
}

pub fn deliver(
    conn: &Connection,
    webhook: &Webhook,
    event_type: &str,
    action: &str,
    data: serde_json::Value,
) -> Result<()> {
    // Re-validate the stored URL against the SSRF ranges on every send: a
    // hostname that passed validation at create time can later be repointed
    // at an internal address (DNS rebinding), and delivery — not just
    // registration — is the moment that actually reaches the network.
    if let Err(e) = validate_webhook_url(&webhook.url) {
        return log_delivery(
            conn,
            webhook,
            event_type,
            b"",
            "blocked_ssrf",
            &e.to_string(),
        );
    }

    let payload = serde_json::json!({
        "event": event_type,
        "action": action,
        "webhook_id": webhook.id,
        "workspace_id": webhook.workspace_id,
        "data": data,
    });
    let body = serde_json::to_vec(&payload)
        .map_err(|e| Error::Database(rusqlite::Error::ToSqlConversionFailure(Box::new(e))))?;
    let signature = sign(&webhook.secret_key, &body);
    let delivery_id = uuid::Uuid::new_v4().to_string();

    let request = http_agent()
        .post(&webhook.url)
        .set("Content-Type", "application/json")
        .set("User-Agent", "agentflare-backend")
        .set("X-Agentflare-Delivery", &delivery_id)
        .set("X-Agentflare-Event", event_type)
        .set("X-Agentflare-Signature", &signature)
        .timeout(std::time::Duration::from_secs(5));

    let (status, resp_body) = match request.send_bytes(&body) {
        Ok(resp) => (resp.status().to_string(), capped_body(resp)),
        Err(ureq::Error::Status(code, resp)) => (code.to_string(), capped_body(resp)),
        Err(e) => ("connection_error".to_string(), e.to_string()),
    };

    log_delivery(conn, webhook, event_type, &body, &status, &resp_body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::workspace::{self, CreateWorkspace};

    fn seed_workspace(conn: &Connection) -> String {
        workspace::create(
            conn,
            CreateWorkspace {
                name: "Test".into(),
                slug: "test".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap()
        .id
    }

    #[test]
    fn sign_produces_known_hmac() {
        let result = sign("mysecret", b"hello");
        assert_eq!(result.len(), 64);
        // Regression pin: known output for "mysecret" + "hello"
        assert_eq!(
            result,
            "f09399f0c446d84b31a080e57ec483392d41e6f512f3e7ada5027abbcd358c2a"
        );
    }

    #[test]
    fn validate_url_rejects_bad() {
        assert!(validate_webhook_url("ftp://example.com").is_err());
        assert!(validate_webhook_url("http://localhost/hook").is_err());
        assert!(validate_webhook_url("http://127.0.0.1/hook").is_err());
        assert!(validate_webhook_url("http://0.0.0.0/hook").is_err());
        assert!(validate_webhook_url("http://[::1]/hook").is_err());
        assert!(validate_webhook_url("not-a-url").is_err());
    }

    #[test]
    fn domain_resolution_flags_names_pointing_at_loopback() {
        // "localhost" is in every /etc/hosts and resolves to a loopback
        // address offline — a stand-in for any external-looking hostname an
        // attacker points at an internal IP, which the IP-literal checks
        // above can't see. Resolution is the layer that catches it.
        assert!(domain_resolves_to_blocked_ip("localhost", 80));
    }

    #[test]
    fn validate_url_accepts_valid() {
        assert!(validate_webhook_url("https://example.com/hook").is_ok());
        assert!(validate_webhook_url("http://hooks.example.com/path").is_ok());
    }

    #[test]
    fn validate_url_rejects_private_and_link_local() {
        assert!(validate_webhook_url("http://169.254.169.254/").is_err());
        assert!(validate_webhook_url("http://10.0.0.1/").is_err());
        assert!(validate_webhook_url("http://172.16.0.5/").is_err());
        assert!(validate_webhook_url("http://192.168.1.1/").is_err());
        assert!(validate_webhook_url("http://[fe80::1]/").is_err());
        assert!(validate_webhook_url("http://[fc00::1]/").is_err());
    }

    #[test]
    fn create_validates_url() {
        let conn = db::open_in_memory().unwrap();
        let wid = seed_workspace(&conn);
        let err = create(
            &conn,
            CreateWebhook {
                workspace_id: wid.clone(),
                url: "http://localhost/hook".into(),
                secret_key: "k".into(),
                on_item: Some(true),
                on_state: None,
                on_project: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidTransition(_)));
    }

    #[test]
    fn create_and_get() {
        let conn = db::open_in_memory().unwrap();
        let wid = seed_workspace(&conn);
        let wh = create(
            &conn,
            CreateWebhook {
                workspace_id: wid,
                url: "https://example.com/hook".into(),
                secret_key: "s3cret".into(),
                on_item: Some(true),
                on_state: None,
                on_project: None,
            },
        )
        .unwrap();
        assert!(wh.is_active);
        assert!(wh.on_item);
        let got = get(&conn, &wh.id).unwrap();
        assert_eq!(got.id, wh.id);
    }

    #[test]
    fn list_webhooks_by_workspace() {
        let conn = db::open_in_memory().unwrap();
        let wid = seed_workspace(&conn);
        create(
            &conn,
            CreateWebhook {
                workspace_id: wid.clone(),
                url: "https://a.com/hook".into(),
                secret_key: "k1".into(),
                on_item: Some(true),
                on_state: None,
                on_project: None,
            },
        )
        .unwrap();
        create(
            &conn,
            CreateWebhook {
                workspace_id: wid.clone(),
                url: "https://b.com/hook".into(),
                secret_key: "k2".into(),
                on_item: None,
                on_state: Some(true),
                on_project: None,
            },
        )
        .unwrap();
        assert_eq!(list_by_workspace(&conn, &wid).unwrap().len(), 2);
    }

    #[test]
    fn list_active_matching_filters_correctly() {
        let conn = db::open_in_memory().unwrap();
        let wid = seed_workspace(&conn);
        create(
            &conn,
            CreateWebhook {
                workspace_id: wid.clone(),
                url: "https://a.com/hook".into(),
                secret_key: "k1".into(),
                on_item: Some(true),
                on_state: None,
                on_project: None,
            },
        )
        .unwrap();
        create(
            &conn,
            CreateWebhook {
                workspace_id: wid.clone(),
                url: "https://b.com/hook".into(),
                secret_key: "k2".into(),
                on_item: None,
                on_state: Some(true),
                on_project: None,
            },
        )
        .unwrap();
        let item_hooks = list_active_matching(&conn, &wid, "item").unwrap();
        assert_eq!(item_hooks.len(), 1);
        let state_hooks = list_active_matching(&conn, &wid, "state").unwrap();
        assert_eq!(state_hooks.len(), 1);
        let project_hooks = list_active_matching(&conn, &wid, "project").unwrap();
        assert!(project_hooks.is_empty());
    }

    #[test]
    fn deliver_failure_logs_and_does_not_panic() {
        let conn = db::open_in_memory().unwrap();
        let wid = seed_workspace(&conn);
        let wh = create(
            &conn,
            CreateWebhook {
                workspace_id: wid,
                url: "https://example.invalid/hook".into(),
                secret_key: "s3cret".into(),
                on_item: Some(true),
                on_state: None,
                on_project: None,
            },
        )
        .unwrap();
        let result = deliver(
            &conn,
            &wh,
            "item",
            "create",
            serde_json::json!({"id": "123"}),
        );
        assert!(result.is_ok(), "delivery failure must not propagate as Err");
        let log_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM webhook_logs WHERE webhook_id = ?1",
                params![wh.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(log_count, 1, "a log row must be created even on failure");
    }

    #[test]
    fn list_logs_by_workspace_returns_delivery_history() {
        let conn = db::open_in_memory().unwrap();
        let wid = seed_workspace(&conn);
        let wh = create(
            &conn,
            CreateWebhook {
                workspace_id: wid.clone(),
                url: "https://example.invalid/hook".into(),
                secret_key: "s3cret".into(),
                on_item: Some(true),
                on_state: None,
                on_project: None,
            },
        )
        .unwrap();
        deliver(
            &conn,
            &wh,
            "item",
            "create",
            serde_json::json!({"id": "123"}),
        )
        .unwrap();
        let logs = list_logs_by_workspace(&conn, &wid).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].webhook_id, wh.id);
        assert_eq!(logs[0].event_type.as_deref(), Some("item"));
        let empty = list_logs_by_workspace(&conn, "nonexistent-workspace").unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn delete_soft() {
        let conn = db::open_in_memory().unwrap();
        let wid = seed_workspace(&conn);
        let wh = create(
            &conn,
            CreateWebhook {
                workspace_id: wid,
                url: "https://example.com/hook".into(),
                secret_key: "s3cret".into(),
                on_item: None,
                on_state: None,
                on_project: None,
            },
        )
        .unwrap();
        delete(&conn, &wh.id).unwrap();
        assert!(matches!(get(&conn, &wh.id), Err(Error::NotFound(_))));
    }
}
