use crate::webhook;
use rusqlite::Connection;
use serde_json::Value;

pub fn emit(conn: &Connection, workspace_id: &str, event_type: &str, action: &str, data: Value) {
    let webhooks = match webhook::list_active_matching(conn, workspace_id, event_type) {
        Ok(hooks) => hooks,
        Err(_) => return,
    };
    for wh in webhooks {
        let _ = webhook::deliver(conn, &wh, event_type, action, data.clone());
    }
}
