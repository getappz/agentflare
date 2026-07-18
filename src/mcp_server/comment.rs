//! `comment` MCP tool handler body -- split out of mcp_server.rs (item #168).

use super::*;

impl AgentflareMcp {
    pub fn comment_impl(&self, req: CommentRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "create" => {
                let item_id = req.item_id.ok_or_else(|| {
                    ErrorData::invalid_params("item_id is required for create", None)
                })?;
                let body = req.body.ok_or_else(|| {
                    ErrorData::invalid_params("body is required for create", None)
                })?;
                if item_id.trim().is_empty() || body.trim().is_empty() {
                    return Err(ErrorData::invalid_params(
                        "item_id and body are required",
                        None,
                    ));
                }
                let author = crate::claims::owner_id();
                self.with_backend_db(|conn| {
                    let comment =
                        agentflare_backend::comment::create(conn, &item_id, &author, &body)
                            .map_err(map_backend_err)?;
                    Ok(serde_json::to_string_pretty(&comment).unwrap_or_default())
                })?
            }
            "edit" => {
                let comment_id = req
                    .id
                    .ok_or_else(|| ErrorData::invalid_params("id is required for edit", None))?;
                let body = req
                    .body
                    .ok_or_else(|| ErrorData::invalid_params("body is required for edit", None))?;
                if comment_id.trim().is_empty() || body.trim().is_empty() {
                    return Err(ErrorData::invalid_params("id and body are required", None));
                }
                let owner = crate::claims::owner_id();
                let now = crate::claims::now();
                let ttl = backend_claim_ttl_secs();
                self.with_backend_db(|conn| {
                    // The author/latest/claim checks and the write must be one
                    // transaction — otherwise a comment landing between the
                    // is_latest check and the write (routine under concurrent
                    // multi-agent access) can silently violate the
                    // "only the latest comment is editable" invariant.
                    let tx = conn
                        .unchecked_transaction()
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    let comment = agentflare_backend::comment::get(&tx, &comment_id)
                        .map_err(map_backend_err)?;
                    if crate::claims::agent_of(&comment.author_agent)
                        != crate::claims::agent_of(&owner)
                    {
                        return Err(ErrorData::invalid_params(
                            "can only edit your own comments",
                            None,
                        ));
                    }
                    if !agentflare_backend::comment::is_latest(&tx, &comment)
                        .map_err(map_backend_err)?
                    {
                        return Err(ErrorData::invalid_params(
                            "comment is not the latest on this item — cannot edit",
                            None,
                        ));
                    }
                    if agentflare_backend::claim::has_active_claim_by_other(
                        &tx,
                        &comment.item_id,
                        &owner,
                        now,
                        ttl,
                    )
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                    {
                        return Err(ErrorData::invalid_params(
                            "another agent has started work on this item — cannot edit",
                            None,
                        ));
                    }
                    let updated = agentflare_backend::comment::update(&tx, &comment_id, &body)
                        .map_err(map_backend_err)?;
                    tx.commit()
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    Ok(serde_json::to_string_pretty(&updated).unwrap_or_default())
                })?
            }
            "delete" => {
                let comment_id = req
                    .id
                    .ok_or_else(|| ErrorData::invalid_params("id is required for delete", None))?;
                if comment_id.trim().is_empty() {
                    return Err(ErrorData::invalid_params("id is required", None));
                }
                let owner = crate::claims::owner_id();
                let now = crate::claims::now();
                let ttl = backend_claim_ttl_secs();
                self.with_backend_db(|conn| {
                    // See "edit" above: checks + write must be one transaction
                    // to close the same TOCTOU window.
                    let tx = conn
                        .unchecked_transaction()
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    let comment = agentflare_backend::comment::get(&tx, &comment_id)
                        .map_err(map_backend_err)?;
                    if crate::claims::agent_of(&comment.author_agent)
                        != crate::claims::agent_of(&owner)
                    {
                        return Err(ErrorData::invalid_params(
                            "can only delete your own comments",
                            None,
                        ));
                    }
                    if !agentflare_backend::comment::is_latest(&tx, &comment)
                        .map_err(map_backend_err)?
                    {
                        return Err(ErrorData::invalid_params(
                            "comment is not the latest on this item — cannot delete",
                            None,
                        ));
                    }
                    if agentflare_backend::claim::has_active_claim_by_other(
                        &tx,
                        &comment.item_id,
                        &owner,
                        now,
                        ttl,
                    )
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                    {
                        return Err(ErrorData::invalid_params(
                            "another agent has started work on this item — cannot delete",
                            None,
                        ));
                    }
                    agentflare_backend::comment::delete(&tx, &comment_id)
                        .map_err(map_backend_err)?;
                    tx.commit()
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    Ok(serde_json::json!({"deleted": true, "id": comment_id}).to_string())
                })?
            }
            "list" => {
                let item_id = req.item_id.ok_or_else(|| {
                    ErrorData::invalid_params("item_id is required for list", None)
                })?;
                if item_id.trim().is_empty() {
                    return Err(ErrorData::invalid_params("item_id is required", None));
                }
                self.with_backend_db(|conn| {
                    let comments = agentflare_backend::comment::list_by_item(conn, &item_id)
                        .map_err(map_backend_err)?;
                    Ok(serde_json::to_string_pretty(&comments).unwrap_or_default())
                })?
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown comment action: '{other}' — expected create|edit|delete|list"),
                None,
            )),
        }
    }
}
