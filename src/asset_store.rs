use agentflare_store::Store;

pub fn entity_path(entity_type: &str, entity_id: &str, filename: &str) -> String {
    format!("{entity_type}/{entity_id}/{filename}")
}

pub fn parse_entity_path(path: &str) -> Option<(&str, &str, &str)> {
    let mut parts = path.splitn(3, '/');
    let entity_type = parts.next()?;
    let entity_id = parts.next()?;
    let filename = parts.next()?;
    if entity_type.is_empty() || entity_id.is_empty() || filename.is_empty() {
        return None;
    }
    Some((entity_type, entity_id, filename))
}

pub fn document_to_asset_json(
    doc: &agentflare_store::documents::Document,
) -> serde_json::Value {
    let (entity_type, entity_id, filename) = parse_entity_path(&doc.path)
        .unwrap_or(("unknown", "unknown", &doc.path));
    let meta: serde_json::Value =
        serde_json::from_str(&doc.metadata).unwrap_or(serde_json::Value::Object(Default::default()));
    serde_json::json!({
        "id": doc.id,
        "workspace_id": doc.project_id,
        "entity_type": entity_type,
        "entity_id": entity_id,
        "filename": filename,
        "size": doc.size,
        "mime_type": doc.mime,
        "metadata": meta,
        "created_at": doc.created_at,
        "updated_at": doc.updated_at,
        "deleted_at": doc.deleted_at,
        "version": doc.version,
    })
}

pub fn backfill_legacy_assets(
    store: &Store,
    backend_conn: &rusqlite::Connection,
    asset_base_path: &std::path::Path,
) -> Result<usize, Box<dyn std::error::Error>> {
    if let Some(marker) = store.kv_get("_asset_backfill_done")? {
        let ts: i64 = serde_json::from_slice(&marker.value)?;
        return Err(format!("backfill already ran at {ts}").into());
    }

    let assets = agentflare_backend::asset::list_all(backend_conn)?;
    if assets.is_empty() {
        let now = db_kit::ids::now();
        store.kv_set(
            "_asset_backfill_done",
            &serde_json::to_vec(&now)?,
        )?;
        return Ok(0);
    }

    for asset in &assets {
        let path = entity_path(&asset.entity_type, &asset.entity_id, &asset.filename);
        let bytes = agentflare_backend::asset::read_file(asset_base_path, &asset.storage_path)?;
        let blob_hash = store.blob_store(&bytes)?;

        store.doc_upsert_with_opts(
            &asset.workspace_id.clone().unwrap_or_default(),
            &path,
            "",
            agentflare_store::documents::DocUpsertOpts {
                title: Some(asset.filename.clone()),
                doc_type: Some("asset".into()),
                blob_hash: Some(blob_hash),
                mime: Some(asset.mime_type.clone().unwrap_or_default()),
                source: Some("backfill".into()),
                metadata: Some(asset.metadata.clone()),
                size: Some(asset.size),
                ..Default::default()
            },
        )?;
    }

    let now = db_kit::ids::now();
    store.kv_set(
        "_asset_backfill_done",
        &serde_json::to_vec(&now)?,
    )?;

    Ok(assets.len())
}

pub fn get_blob_content(
    store: &Store,
    doc: &agentflare_store::documents::Document,
) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error>> {
    match &doc.blob_hash {
        Some(hash) => Ok(store.blob_get(hash)?),
        None => {
            let content = doc.content.as_bytes().to_vec();
            Ok(Some(content))
        }
    }
}
