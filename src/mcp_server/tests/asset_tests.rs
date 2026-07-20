use super::*;

#[test]
fn asset_attach_get_list_delete_round_trip() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let home = crate::paths::home();
        let staging = home.join(".agentflare").join("staging");
        std::fs::create_dir_all(&staging).unwrap();

        let item: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("asset-test"))).unwrap())
                .unwrap();
        let item_id = item["id"].as_str().unwrap().to_string();

        let content = b"hello asset test";
        std::fs::write(staging.join("test.txt"), content).unwrap();

        let attached: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: Some(item_id.clone()),
                project_id: None,
                filename: Some("test.txt".into()),
                metadata: Some(r#"{"source":"test"}"#.into()),
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(attached["filename"], "test.txt");
        let asset_id = attached["id"].as_str().unwrap().to_string();

        let got: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "get".into(),
                id: Some(asset_id.clone()),
                item_id: None,
                project_id: None,
                filename: None,
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(got["asset"]["filename"], "test.txt");
        assert!(got["content"].as_str().is_some());

        let list: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "list".into(),
                id: None,
                item_id: Some(item_id.clone()),
                project_id: None,
                filename: None,
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["id"], asset_id);

        let del: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "delete".into(),
                id: Some(asset_id.clone()),
                item_id: None,
                project_id: None,
                filename: None,
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(del["deleted"], true);

        let after: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "list".into(),
                id: None,
                item_id: Some(item_id),
                project_id: None,
                filename: None,
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(after.as_array().unwrap().is_empty());
    });
}

#[test]
fn asset_attach_rejects_path_traversal() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let err = s
            .asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: Some("item-1".into()),
                project_id: None,
                filename: Some("../etc/hosts".into()),
                metadata: None,
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    });
}

#[test]
fn asset_attach_rejects_missing_filename() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let err = s
            .asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: Some("item-1".into()),
                project_id: None,
                filename: None,
                metadata: None,
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    });
}

#[test]
fn asset_attach_rejects_both_item_and_project() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let err = s
            .asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: Some("item-1".into()),
                project_id: Some("proj-1".into()),
                filename: Some("anything.txt".into()),
                metadata: None,
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    });
}

#[test]
fn asset_get_rejects_missing_id() {
    let (_tmp, s) = harness();
    let err = s
        .asset(Parameters(AssetRequest {
            action: "get".into(),
            id: None,
            item_id: None,
            project_id: None,
            filename: None,
            metadata: None,
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn asset_get_and_delete_after_delete_return_not_found() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let home = crate::paths::home();
        let staging = home.join(".agentflare").join("staging");
        std::fs::create_dir_all(&staging).unwrap();

        let item: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("gone"))).unwrap()).unwrap();
        let item_id = item["id"].as_str().unwrap().to_string();

        std::fs::write(staging.join("gone.txt"), b"bye").unwrap();
        let attached: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: Some(item_id),
                project_id: None,
                filename: Some("gone.txt".into()),
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();
        let asset_id = attached["id"].as_str().unwrap().to_string();

        s.asset(Parameters(AssetRequest {
            action: "delete".into(),
            id: Some(asset_id.clone()),
            item_id: None,
            project_id: None,
            filename: None,
            metadata: None,
        }))
        .unwrap();

        // A deleted asset must not be gettable, matching the pre-#185
        // agentflare_backend::asset::get contract (deleted_at IS NULL).
        s.asset(Parameters(AssetRequest {
            action: "get".into(),
            id: Some(asset_id.clone()),
            item_id: None,
            project_id: None,
            filename: None,
            metadata: None,
        }))
        .unwrap_err();

        // Deleting an already-deleted asset must also report not-found,
        // not silently double-unref an already-purged blob.
        s.asset(Parameters(AssetRequest {
            action: "delete".into(),
            id: Some(asset_id),
            item_id: None,
            project_id: None,
            filename: None,
            metadata: None,
        }))
        .unwrap_err();
    });
}

#[test]
fn asset_shared_storage_delete_safety() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let home = crate::paths::home();
        let staging = home.join(".agentflare").join("staging");
        std::fs::create_dir_all(&staging).unwrap();

        let item1: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("shared-1"))).unwrap())
                .unwrap();
        let item2: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("shared-2"))).unwrap())
                .unwrap();
        let id1 = item1["id"].as_str().unwrap().to_string();
        let id2 = item2["id"].as_str().unwrap().to_string();

        let content = b"same content for shared delete test";
        std::fs::write(staging.join("shared.txt"), content).unwrap();
        let asset1: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: Some(id1.clone()),
                project_id: None,
                filename: Some("shared.txt".into()),
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();
        let a1_id = asset1["id"].as_str().unwrap().to_string();

        // re-stage the same content for item2
        std::fs::write(staging.join("shared.txt"), content).unwrap();
        let asset2: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: Some(id2.clone()),
                project_id: None,
                filename: Some("shared.txt".into()),
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();
        let a2_id = asset2["id"].as_str().unwrap().to_string();

        // delete first — second should still be readable
        let del1: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "delete".into(),
                id: Some(a1_id.clone()),
                item_id: None,
                project_id: None,
                filename: None,
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(del1["deleted"], true);

        let got2_raw = s
            .asset(Parameters(AssetRequest {
                action: "get".into(),
                id: Some(a2_id.clone()),
                item_id: None,
                project_id: None,
                filename: None,
                metadata: None,
            }))
            .unwrap();
        let got2: serde_json::Value = serde_json::from_str(&got2_raw).unwrap();
        assert!(
            got2["content"].as_str().is_some(),
            "item2 must still be readable after item1 deletion: {got2_raw}"
        );

        // delete second — now file should be gone
        let del2: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "delete".into(),
                id: Some(a2_id.clone()),
                item_id: None,
                project_id: None,
                filename: None,
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(del2["deleted"], true);
    });
}

// asset_content_dedup was removed in #185: content dedup via store_blobs
// (blob_store/blob_unref) is tested by the agentflare-store crate itself.

#[test]
fn asset_attach_to_project() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let home = crate::paths::home();
        let staging = home.join(".agentflare").join("staging");
        std::fs::create_dir_all(&staging).unwrap();

        let project: serde_json::Value = serde_json::from_str(
            &s.project(Parameters(ProjectRequest {
                action: "info".into(),
            }))
            .unwrap(),
        )
        .unwrap();
        let project_id = project["id"].as_str().unwrap().to_string();

        std::fs::write(staging.join("project-file.txt"), b"project attachment").unwrap();
        let attached: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: None,
                project_id: Some(project_id.clone()),
                filename: Some("project-file.txt".into()),
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(attached["filename"], "project-file.txt");

        let list: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "list".into(),
                id: None,
                item_id: None,
                project_id: Some(project_id),
                filename: None,
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
    });
}

#[test]
fn asset_attach_rejects_neither_item_nor_project() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let err = s
            .asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: None,
                project_id: None,
                filename: Some("anything.txt".into()),
                metadata: None,
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    });
}

#[test]
fn asset_attach_rejects_nonexistent_item() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let home = crate::paths::home();
        let staging = home.join(".agentflare").join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("f.txt"), b"data").unwrap();
        let err = s
            .asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: Some("nonexistent-item".into()),
                project_id: None,
                filename: Some("f.txt".into()),
                metadata: None,
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    });
}

#[test]
fn asset_attach_rejects_missing_staging_file() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let err = s
            .asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: Some("item-1".into()),
                project_id: None,
                filename: Some("does-not-exist.txt".into()),
                metadata: None,
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    });
}

#[test]
fn asset_attach_rejects_oversized_file() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let home = crate::paths::home();
        let staging = home.join(".agentflare").join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        // write a file just past the default 5 MB limit
        let big = vec![0u8; 5 * 1024 * 1024 + 1];
        std::fs::write(staging.join("big.bin"), &big).unwrap();
        let err = s
            .asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: Some("item-1".into()),
                project_id: None,
                filename: Some("big.bin".into()),
                metadata: None,
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    });
}

#[test]
fn asset_get_over_max_inline_omits_content() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let home = crate::paths::home();
        let staging = home.join(".agentflare").join("staging");
        std::fs::create_dir_all(&staging).unwrap();

        let item: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(empty_item_create("big-inline-test")))
                .unwrap(),
        )
        .unwrap();
        let item_id = item["id"].as_str().unwrap().to_string();

        // write a small file, but set a tiny inline cap for this test
        std::fs::write(staging.join("small.txt"), b"hello inline cap").unwrap();
        let attached: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: Some(item_id.clone()),
                project_id: None,
                filename: Some("small.txt".into()),
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();
        let asset_id = attached["id"].as_str().unwrap().to_string();

        // override inline limit to 1 byte so our file exceeds it
        // SAFETY: with_temp_home holds GLOBAL_STATE_LOCK so no concurrent env mutation.
        let saved = std::env::var("AGENTFLARE_BACKEND_ASSET_MAX_INLINE_BYTES").ok();
        unsafe { std::env::set_var("AGENTFLARE_BACKEND_ASSET_MAX_INLINE_BYTES", "1") };
        let got: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "get".into(),
                id: Some(asset_id),
                item_id: None,
                project_id: None,
                filename: None,
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(got["content"].is_null());
        assert!(got["content_omitted_reason"].as_str().is_some());
        // restore to avoid leaking to sibling tests
        match saved {
            Some(v) => unsafe { std::env::set_var("AGENTFLARE_BACKEND_ASSET_MAX_INLINE_BYTES", v) },
            None => unsafe { std::env::remove_var("AGENTFLARE_BACKEND_ASSET_MAX_INLINE_BYTES") },
        }
    });
}

#[test]
fn asset_get_returns_text_content_as_utf8_not_base64() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let home = crate::paths::home();
        let staging = home.join(".agentflare").join("staging");
        std::fs::create_dir_all(&staging).unwrap();

        let item: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(empty_item_create("utf8-content-test")))
                .unwrap(),
        )
        .unwrap();
        let item_id = item["id"].as_str().unwrap().to_string();

        let body = "# Handoff\n\nImplement the fix \u{2192} land a PR. \u{2713}";
        std::fs::write(staging.join("note.md"), body.as_bytes()).unwrap();
        let attached: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: Some(item_id.clone()),
                project_id: None,
                filename: Some("note.md".into()),
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();
        let asset_id = attached["id"].as_str().unwrap().to_string();

        let got: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "get".into(),
                id: Some(asset_id),
                item_id: None,
                project_id: None,
                filename: None,
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();

        // Text assets must come back as readable UTF-8, not base64.
        assert_eq!(got["encoding"].as_str(), Some("utf8"));
        assert_eq!(got["content"].as_str(), Some(body));
    });
}

#[test]
fn asset_get_returns_base64_for_binary_with_valid_utf8_bytes() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let home = crate::paths::home();
        let staging = home.join(".agentflare").join("staging");
        std::fs::create_dir_all(&staging).unwrap();

        let item: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(empty_item_create("binary-utf8-test")))
                .unwrap(),
        )
        .unwrap();
        let item_id = item["id"].as_str().unwrap().to_string();

        // Bytes 0x00,0x01,0x02,0x03 are valid UTF-8, but this is an
        // octet-stream (.bin) asset: it must come back Base64, not "utf8".
        let raw = [0u8, 1, 2, 3];
        assert!(
            std::str::from_utf8(&raw).is_ok(),
            "precondition: valid UTF-8"
        );
        std::fs::write(staging.join("blob.bin"), raw).unwrap();
        let attached: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: Some(item_id.clone()),
                project_id: None,
                filename: Some("blob.bin".into()),
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();
        let asset_id = attached["id"].as_str().unwrap().to_string();

        let got: serde_json::Value = serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "get".into(),
                id: Some(asset_id),
                item_id: None,
                project_id: None,
                filename: None,
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap();

        // Binary MIME => Base64 regardless of UTF-8 validity.
        assert_eq!(got["encoding"].as_str(), Some("base64"));
        // Content must be the Base64 of the raw bytes, not merely labeled so.
        assert_eq!(got["content"].as_str(), Some("AAECAw=="));
    });
}
