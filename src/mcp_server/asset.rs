//! `asset` MCP tool handler body -- split out of mcp_server.rs (item #168).

use super::*;

impl AgentflareMcp {
    pub fn asset_impl(
        &self,
        AssetRequest {
            action,
            id,
            item_id,
            project_id,
            filename,
            metadata,
        }: AssetRequest,
    ) -> Result<String, ErrorData> {
        match action.as_str() {
            "attach" => {
                let has_item = item_id.is_some();
                let has_project = project_id.is_some();
                if has_item == has_project {
                    return Err(ErrorData::invalid_params(
                        "exactly one of item_id or project_id is required for attach",
                        None,
                    ));
                }
                let fn_val = filename.ok_or_else(|| {
                    ErrorData::invalid_params("filename is required for attach", None)
                })?;
                // path traversal guard: reject filename with .. or absolute components
                let staged_rel = std::path::Path::new(&fn_val);
                if staged_rel
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
                {
                    return Err(ErrorData::invalid_params(
                        format!(
                            "filename '{fn_val}' contains path separators or parent-refs — not allowed"
                        ),
                        None,
                    ));
                }
                let staging_dir = crate::paths::home().join(".agentflare").join("staging");
                let staged = staging_dir.join(&fn_val);
                if !staged.exists() {
                    return Err(ErrorData::invalid_params(
                        format!(
                            "file not found at staging path: {} — write the file there before calling attach",
                            staged.display()
                        ),
                        None,
                    ));
                }
                let size = std::fs::metadata(&staged)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                    .len();
                let max_attach = Self::asset_max_attach_bytes();
                if size > max_attach {
                    return Err(ErrorData::invalid_params(
                        format!(
                            "file is {} bytes, exceeds the {} byte attach limit",
                            size, max_attach
                        ),
                        None,
                    ));
                }
                let bytes = std::fs::read(&staged)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let hash = Self::content_hash(&bytes);
                let meta = metadata.unwrap_or_else(|| "{}".to_string());
                self.with_backend_db(|conn| {
                    let ws_id = Self::resolve_workspace_id(conn)?;
                    let (entity_type, entity_id) = if has_item {
                        agentflare_backend::item::get(conn, item_id.as_ref().unwrap())
                            .map_err(map_backend_err)?;
                        ("item_attachment", item_id.as_ref().unwrap().clone())
                    } else {
                        agentflare_backend::project::get(conn, project_id.as_ref().unwrap())
                            .map_err(map_backend_err)?;
                        ("project_attachment", project_id.as_ref().unwrap().clone())
                    };
                    let ext = std::path::Path::new(&fn_val)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("");
                    let mime = Self::infer_mime_type(ext);
                    let stem = std::path::Path::new(&fn_val)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(&fn_val);
                    let safe_stem: String = {
                        let s: String = stem
                            .chars()
                            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                            .collect();
                        if s.is_empty() { "file".to_string() } else { s }
                    };
                    let full_storage = if ext.is_empty() {
                        format!("{}/assets/{}-{}", ws_id, safe_stem, hash)
                    } else {
                        format!("{}/assets/{}-{}.{}", ws_id, safe_stem, hash, ext)
                    };
                    let base_path = crate::paths::home().join(".agentflare");
                    // only write if file doesn't already exist (same content already stored)
                    let target = base_path.join(&full_storage);
                    if !target.exists() {
                        agentflare_backend::asset::write_file(&base_path, &full_storage, &bytes)
                            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    }
                    let asset = agentflare_backend::asset::create(
                        conn,
                        agentflare_backend::asset::CreateAsset {
                            workspace_id: Some(ws_id.clone()),
                            entity_type: entity_type.into(),
                            entity_id,
                            filename: fn_val.clone(),
                            size: size as i64,
                            mime_type: Some(mime),
                            metadata: Some(meta),
                            storage_path: Some(full_storage),
                        },
                    )
                    .map_err(map_backend_err)?;
                    // remove staging file only after the DB insert succeeds
                    let _ = std::fs::remove_file(&staged);
                    Ok(
                        serde_json::to_string_pretty(&Self::strip_storage_path(&asset))
                            .unwrap_or_default(),
                    )
                })?
            }
            "get" => {
                let id =
                    id.ok_or_else(|| ErrorData::invalid_params("id is required for get", None))?;
                self.with_backend_db(|conn| {
                    let asset = agentflare_backend::asset::get(conn, &id)
                        .map_err(map_backend_err)?;
                    let base_path = crate::paths::home().join(".agentflare");
                    let max_inline = Self::asset_max_inline_bytes();
                    let meta = Self::strip_storage_path(&asset);
                    let size = asset.size as u64;
                    if size <= max_inline {
                        match agentflare_backend::asset::read_file(&base_path, &asset.storage_path) {
                            Ok(bytes) => {
                                // Textual MIME + valid UTF-8 => return readable text so
                                // callers don't decode every text asset; everything else
                                // (binary MIME, or invalid UTF-8) => Base64.
                                let (content, encoding) = match std::str::from_utf8(&bytes) {
                                    Ok(text) if Self::mime_is_textual(asset.mime_type.as_deref()) => (text.to_string(), "utf8"),
                                    _ => (base64_encode(&bytes), "base64"),
                                };
                                let result = serde_json::json!({
                                    "asset": meta,
                                    "content": content,
                                    "encoding": encoding,
                                });
                                Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
                            }
                            Err(e) => {
                                let result = serde_json::json!({
                                    "asset": meta,
                                    "content": null,
                                    "content_omitted_reason": format!("could not read file: {}", e),
                                });
                                Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
                            }
                        }
                    } else {
                        let result = serde_json::json!({
                            "asset": meta,
                            "content": null,
                            "content_omitted_reason": format!("file is {} bytes, exceeds the {} byte inline limit", size, max_inline),
                        });
                        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
                    }
                })?
            }
            "list" => self.with_backend_db(|conn| {
                let ws_id = Self::resolve_workspace_id(conn)?;
                let assets: Vec<agentflare_backend::asset::Asset> = match (item_id, project_id) {
                    (Some(iid), None) => {
                        agentflare_backend::asset::list_by_entity(conn, "item_attachment", &iid)
                            .map_err(map_backend_err)?
                    }
                    (None, Some(pid)) => {
                        agentflare_backend::asset::list_by_entity(conn, "project_attachment", &pid)
                            .map_err(map_backend_err)?
                    }
                    (Some(_), Some(_)) => {
                        return Err(ErrorData::invalid_params(
                            "only one of item_id or project_id allowed for list, not both",
                            None,
                        ));
                    }
                    (None, None) => {
                        let mut assets: Vec<serde_json::Value> = Vec::new();
                        for a in agentflare_backend::asset::list_by_workspace(conn, &ws_id)
                            .map_err(map_backend_err)?
                        {
                            assets.push(Self::strip_storage_path(&a));
                        }
                        return Ok(serde_json::to_string_pretty(&assets).unwrap_or_default());
                    }
                };
                let mut stripped: Vec<serde_json::Value> = Vec::new();
                for a in assets {
                    stripped.push(Self::strip_storage_path(&a));
                }
                Ok(serde_json::to_string_pretty(&stripped).unwrap_or_default())
            })?,
            "delete" => {
                let id =
                    id.ok_or_else(|| ErrorData::invalid_params("id is required for delete", None))?;
                self.with_backend_db(|conn| {
                    let asset = agentflare_backend::asset::get(conn, &id)
                        .map_err(map_backend_err)?;
                    // soft-delete the row
                    agentflare_backend::asset::delete(conn, &id)
                        .map_err(map_backend_err)?;
                    // only unlink from disk if no other live row references the same storage_path
                    let remaining: i64 = conn
                        .query_row(
                            "SELECT count(*) FROM assets WHERE storage_path = ?1 AND deleted_at IS NULL",
                            rusqlite::params![&asset.storage_path],
                            |r| r.get(0),
                        )
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    if remaining == 0 {
                        let base_path = crate::paths::home().join(".agentflare");
                        let _ = agentflare_backend::asset::delete_file(&base_path, &asset.storage_path);
                    }
                    Ok(serde_json::json!({"deleted": true, "id": id}).to_string())
                })?
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action '{other}'; expected attach|get|list|delete"),
                None,
            )),
        }
    }
}
