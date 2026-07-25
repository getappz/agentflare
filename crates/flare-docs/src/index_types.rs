use rustdoc_types::{Id, ItemKind, ItemSummary};
use serde::Deserialize;
use std::collections::HashMap;

/// Trimmed deserialization target for rustdoc JSON's top-level shape.
///
/// Mirrors [`rustdoc_types::Crate`] but keeps only the two maps the indexer
/// reads, and [`IndexItem`] skips the (large, deeply nested)
/// `inner: ItemEnum` field entirely. Serde silently ignores JSON fields
/// absent from the target struct, so this never materializes the per-item
/// signature/generics/impl subtree; item kind comes from `paths[id].kind`
/// instead, which rustdoc populates for exactly the items worth indexing
/// (those with a public, linkable path).
#[derive(Debug, Deserialize)]
pub struct IndexCrate {
    pub index: HashMap<Id, IndexItem>,
    pub paths: HashMap<Id, ItemSummary>,
}

#[derive(Debug, Deserialize)]
pub struct IndexItem {
    pub name: Option<String>,
    pub docs: Option<String>,
}

/// One rustdoc item worth indexing: has both a docstring and a public path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedItem {
    pub fq_path: String,
    pub name: String,
    pub docs: String,
    pub kind: String,
}

/// Extracts every documented, publicly-pathed item from a parsed rustdoc
/// crate. Items without docs (nothing to search) or without a `paths` entry
/// (impl blocks, private/local items) are skipped.
pub fn indexed_items(crate_json: &IndexCrate) -> Vec<IndexedItem> {
    let mut items = Vec::new();
    for (id, item) in &crate_json.index {
        let Some(docs) = item.docs.as_ref().filter(|d| !d.is_empty()) else {
            continue;
        };
        let Some(summary) = crate_json.paths.get(id) else {
            continue;
        };
        let name = item
            .name
            .clone()
            .or_else(|| summary.path.last().cloned())
            .unwrap_or_default();
        items.push(IndexedItem {
            fq_path: summary.path.join("::"),
            name,
            docs: docs.clone(),
            kind: item_kind_str(summary.kind),
        });
    }
    items
}

/// Renders an [`ItemKind`] as the same snake_case string its `Serialize`
/// impl already produces (`#[serde(rename_all = "snake_case")]`), instead
/// of hand-maintaining a match arm per variant that would need updating on
/// every `rustdoc-types` upgrade that adds a kind.
fn item_kind_str(kind: ItemKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u32) -> Id {
        Id(n)
    }

    #[test]
    fn skips_items_without_docs() {
        let mut index = HashMap::new();
        index.insert(
            id(1),
            IndexItem {
                name: Some("Foo".into()),
                docs: None,
            },
        );
        let mut paths = HashMap::new();
        paths.insert(
            id(1),
            ItemSummary {
                crate_id: 0,
                path: vec!["foo".into(), "Foo".into()],
                kind: ItemKind::Struct,
            },
        );
        let krate = IndexCrate { index, paths };
        assert!(indexed_items(&krate).is_empty());
    }

    #[test]
    fn skips_items_with_empty_docs_string() {
        let mut index = HashMap::new();
        index.insert(
            id(1),
            IndexItem {
                name: Some("Foo".into()),
                docs: Some(String::new()),
            },
        );
        let mut paths = HashMap::new();
        paths.insert(
            id(1),
            ItemSummary {
                crate_id: 0,
                path: vec!["Foo".into()],
                kind: ItemKind::Struct,
            },
        );
        let krate = IndexCrate { index, paths };
        assert!(indexed_items(&krate).is_empty());
    }

    #[test]
    fn skips_items_without_a_public_path() {
        let mut index = HashMap::new();
        index.insert(
            id(1),
            IndexItem {
                name: Some("Foo".into()),
                docs: Some("docs".into()),
            },
        );
        let krate = IndexCrate {
            index,
            paths: HashMap::new(),
        };
        assert!(indexed_items(&krate).is_empty());
    }

    #[test]
    fn indexes_a_documented_item_with_fq_path_and_kind() {
        let mut index = HashMap::new();
        index.insert(
            id(1),
            IndexItem {
                name: Some("State".into()),
                docs: Some("Extractor for shared state.".into()),
            },
        );
        let mut paths = HashMap::new();
        paths.insert(
            id(1),
            ItemSummary {
                crate_id: 0,
                path: vec!["axum".into(), "extract".into(), "State".into()],
                kind: ItemKind::Struct,
            },
        );
        let krate = IndexCrate { index, paths };

        let items = indexed_items(&krate);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].fq_path, "axum::extract::State");
        assert_eq!(items[0].name, "State");
        assert_eq!(items[0].docs, "Extractor for shared state.");
        assert_eq!(items[0].kind, "struct");
    }

    #[test]
    fn deserializes_real_shaped_rustdoc_json() {
        let json = r#"{
            "index": {
                "1": { "name": "State", "docs": "Extractor for shared state." },
                "2": { "name": null, "docs": null }
            },
            "paths": {
                "1": { "crate_id": 0, "path": ["axum", "extract", "State"], "kind": "struct" }
            }
        }"#;
        let krate: IndexCrate = serde_json::from_str(json).unwrap();
        let items = indexed_items(&krate);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].fq_path, "axum::extract::State");
        assert_eq!(items[0].kind, "struct");
    }
}
