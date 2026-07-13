use hmac::{Hmac, Mac};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Webhook {
    pub id: String,
    pub workspace_id: String,
    pub url: String,
    pub is_active: bool,
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

fn validate_webhook_url(raw: &str) -> Result<()> {
    let parsed =
        url::Url::parse(raw).map_err(|_| Error::InvalidTransition("invalid webhook URL".into()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::InvalidTransition(
            "webhook URL must use http or https scheme".into(),
        ));
    }
    let host = parsed.host_str().unwrap_or_default();
    if host == "localhost"
        || host == "127.0.0.1"
        || host == "0.0.0.0"
        || host.starts_with("127.")
        || matches!(parsed.host(), Some(url::Host::Ipv6(addr)) if addr.is_loopback())
    {
        return Err(Error::InvalidTransition(
            "webhook URL must not target localhost or loopback address".into(),
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

    let request = ureq::post(&webhook.url)
        .set("Content-Type", "application/json")
        .set("User-Agent", "agentflare-backend")
        .set("X-Agentflare-Delivery", &delivery_id)
        .set("X-Agentflare-Event", event_type)
        .set("X-Agentflare-Signature", &signature)
        .timeout(std::time::Duration::from_secs(5));

    let (status, resp_body) = match request.send_bytes(&body) {
        Ok(resp) => (
            resp.status().to_string(),
            resp.into_string().unwrap_or_default(),
        ),
        Err(ureq::Error::Status(code, resp)) => {
            (code.to_string(), resp.into_string().unwrap_or_default())
        }
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
    fn validate_url_accepts_valid() {
        assert!(validate_webhook_url("https://example.com/hook").is_ok());
        assert!(validate_webhook_url("http://hooks.example.com/path").is_ok());
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
