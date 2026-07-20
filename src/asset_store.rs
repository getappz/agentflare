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

pub fn document_to_asset_json(doc: &agentflare_store::documents::Document) -> serde_json::Value {
    let (entity_type, entity_id, filename) =
        parse_entity_path(&doc.path).unwrap_or(("unknown", "unknown", &doc.path));
    let meta: serde_json::Value = serde_json::from_str(&doc.metadata)
        .unwrap_or(serde_json::Value::Object(Default::default()));
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
    let mut migrated = 0usize;

    // A single unreadable/corrupt legacy asset must not abort the whole
    // batch: that would leave the marker unwritten, so every later
    // with_store() call would replay it -- re-upserting the assets that
    // already migrated fine and bumping their version/history each time.
    for asset in &assets {
        match backfill_one(store, asset_base_path, asset) {
            Ok(()) => migrated += 1,
            Err(e) => eprintln!("[asset-store backfill] skipping asset {}: {e}", asset.id),
        }
    }

    let now = db_kit::ids::now();
    store.kv_set("_asset_backfill_done", &serde_json::to_vec(&now)?)?;

    Ok(migrated)
}

fn backfill_one(
    store: &Store,
    asset_base_path: &std::path::Path,
    asset: &agentflare_backend::asset::Asset,
) -> Result<(), Box<dyn std::error::Error>> {
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
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backfill_skips_bad_asset_and_still_marks_done() {
        let tmp = tempfile::tempdir().unwrap();
        let base_path = tmp.path().join("home");
        std::fs::create_dir_all(&base_path).unwrap();

        let conn = agentflare_backend::db::open_db(&tmp.path().join("backend.db")).unwrap();
        let good_storage = "ws/assets/good-hash.txt";
        agentflare_backend::asset::write_file(&base_path, good_storage, b"hello").unwrap();
        agentflare_backend::asset::create(
            &conn,
            agentflare_backend::asset::CreateAsset {
                workspace_id: Some("ws".into()),
                entity_type: "item_attachment".into(),
                entity_id: "item-good".into(),
                filename: "good.txt".into(),
                size: 5,
                mime_type: Some("text/plain".into()),
                metadata: None,
                storage_path: Some(good_storage.into()),
            },
        )
        .unwrap();
        // No file written for this one -- read_file will fail, simulating a
        // corrupt/missing legacy asset.
        agentflare_backend::asset::create(
            &conn,
            agentflare_backend::asset::CreateAsset {
                workspace_id: Some("ws".into()),
                entity_type: "item_attachment".into(),
                entity_id: "item-bad".into(),
                filename: "bad.txt".into(),
                size: 5,
                mime_type: Some("text/plain".into()),
                metadata: None,
                storage_path: Some("ws/assets/missing-hash.txt".into()),
            },
        )
        .unwrap();

        let store = Store::open_memory().unwrap();
        let migrated = backfill_legacy_assets(&store, &conn, &base_path).unwrap();
        assert_eq!(migrated, 1, "only the readable asset should migrate");

        let docs = store.doc_list("ws").unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].path, "item_attachment/item-good/good.txt");

        // The completion marker must be written even though one asset
        // failed, so a later with_store() call doesn't replay this batch.
        let err = backfill_legacy_assets(&store, &conn, &base_path).unwrap_err();
        assert!(err.to_string().contains("already ran"));
    }
}
