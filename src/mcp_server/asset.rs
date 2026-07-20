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
                let meta = metadata.unwrap_or_else(|| "{}".to_string());

                // attach always nests with_store inside with_backend_db below, which
                // skips the one-time legacy backfill to avoid a non-reentrant-mutex
                // deadlock -- so trigger it here first, while backend_db is unlocked.
                self.with_store(|_| ())?;

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

                    let path = crate::asset_store::entity_path(entity_type, &entity_id, &fn_val);

                    let result =
                        self.with_store(|store| -> Result<serde_json::Value, ErrorData> {
                            let blob_hash = store
                                .blob_store(&bytes)
                                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

                            let doc = store
                                .doc_upsert_with_opts(
                                    &ws_id,
                                    &path,
                                    "",
                                    agentflare_store::documents::DocUpsertOpts {
                                        title: Some(fn_val.clone()),
                                        doc_type: Some("asset".into()),
                                        blob_hash: Some(blob_hash),
                                        mime: Some(mime),
                                        source: Some("attach".into()),
                                        metadata: Some(meta.clone()),
                                        size: Some(size as i64),
                                        ..Default::default()
                                    },
                                )
                                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

                            let _ = std::fs::remove_file(&staged);
                            Ok(crate::asset_store::document_to_asset_json(&doc))
                        })??;

                    Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
                })?
            }
            "get" => {
                let id =
                    id.ok_or_else(|| ErrorData::invalid_params("id is required for get", None))?;
                self.with_store(|store| -> Result<String, ErrorData> {
                    let doc = store
                        .doc_get(&id)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                        .ok_or_else(|| ErrorData::invalid_params("asset not found", None))?;

                    let max_inline = Self::asset_max_inline_bytes();
                    let meta = crate::asset_store::document_to_asset_json(&doc);
                    let size = doc.size as u64;

                    if size <= max_inline {
                        let content = crate::asset_store::get_blob_content(store, &doc)
                            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                        match content {
                            Some(bytes) => {
                                let (content, encoding) = match std::str::from_utf8(&bytes) {
                                    Ok(text) if Self::mime_is_textual(Some(&doc.mime)) => {
                                        (text.to_string(), "utf8")
                                    }
                                    _ => (base64_encode(&bytes), "base64"),
                                };
                                let result = serde_json::json!({
                                    "asset": meta,
                                    "content": content,
                                    "encoding": encoding,
                                });
                                Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
                            }
                            None => {
                                let result = serde_json::json!({
                                    "asset": meta,
                                    "content": null,
                                    "content_omitted_reason": "blob content not found in store",
                                });
                                Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
                            }
                        }
                    } else {
                        let result = serde_json::json!({
                            "asset": meta,
                            "content": null,
                            "content_omitted_reason": format!(
                                "file is {} bytes, exceeds the {} byte inline limit",
                                size, max_inline
                            ),
                        });
                        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
                    }
                })?
            }
            "list" => {
                let ws_id = match self.with_backend_db(Self::resolve_workspace_id) {
                    Ok(Ok(id)) => id,
                    Ok(Err(e)) => return Err(ErrorData::internal_error(e.to_string(), None)),
                    Err(e) => return Err(e),
                };
                let prefix = match (item_id, project_id) {
                    (Some(iid), None) => format!("item_attachment/{iid}"),
                    (None, Some(pid)) => format!("project_attachment/{pid}"),
                    (Some(_), Some(_)) => {
                        return Err(ErrorData::invalid_params(
                            "only one of item_id or project_id allowed for list, not both",
                            None,
                        ));
                    }
                    (None, None) => String::new(),
                };

                self.with_store(|store| -> Result<String, ErrorData> {
                    let docs = store
                        .doc_list(&ws_id)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

                    let filtered: Vec<serde_json::Value> = docs
                        .into_iter()
                        .filter(|d| {
                            d.doc_type == "asset"
                                && (prefix.is_empty() || d.path.starts_with(&prefix))
                        })
                        .map(|d| crate::asset_store::document_to_asset_json(&d))
                        .collect();

                    Ok(serde_json::to_string_pretty(&filtered).unwrap_or_default())
                })?
            }
            "delete" => {
                let id =
                    id.ok_or_else(|| ErrorData::invalid_params("id is required for delete", None))?;
                self.with_store(|store| -> Result<String, ErrorData> {
                    let doc = store
                        .doc_get(&id)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                        .ok_or_else(|| ErrorData::invalid_params("asset not found", None))?;

                    if let Some(ref hash) = doc.blob_hash {
                        store
                            .blob_unref(hash)
                            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    }
                    store
                        .doc_delete(&id)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

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
