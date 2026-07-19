use rusqlite::Connection;

use super::parse::{Mention, MentionKind};

pub struct ResolvedItem {
    pub sequence_id: i64,
    pub name: String,
    pub state: String,
    pub priority: String,
    pub assignee: Option<String>,
}

pub struct ResolvedAsset {
    pub filename: String,
    pub entity_id: String,
    /// `None` when the file is missing, unreadable, or not valid UTF-8 —
    /// callers show metadata only in that case.
    pub content: Option<String>,
}

pub struct SearchHit {
    pub sequence_id: i64,
    pub name: String,
    pub state: String,
}

pub enum ResolvedContent {
    Item(ResolvedItem),
    Asset(ResolvedAsset),
    Search(Vec<SearchHit>),
    NotFound,
}

pub struct ResolvedMention {
    pub kind: MentionKind,
    pub value: String,
    pub content: ResolvedContent,
}

/// Asset content injected into context is truncated here — a mentioned
/// attachment is a summary aid, not a substitute for reading the full file.
const MAX_ASSET_CHARS: usize = 4000;
const SEARCH_LIMIT: usize = 5;

pub fn resolve_mentions(
    conn: &Connection,
    project_id: &str,
    mentions: &[Mention],
) -> Vec<ResolvedMention> {
    mentions
        .iter()
        .map(|m| {
            let content = match m.kind {
                MentionKind::Item => resolve_item(conn, project_id, &m.value),
                MentionKind::Asset => resolve_asset(conn, project_id, &m.value),
                MentionKind::Search => resolve_search(conn, project_id, &m.value),
            };
            ResolvedMention {
                kind: m.kind,
                value: m.value.clone(),
                content,
            }
        })
        .collect()
}

fn state_name(conn: &Connection, state_id: &str) -> String {
    agentflare_backend::state::get(conn, state_id)
        .map(|s| s.name)
        .unwrap_or_else(|_| state_id.to_string())
}

fn resolve_item(conn: &Connection, project_id: &str, id: &str) -> ResolvedContent {
    let Ok(item) = agentflare_backend::item::get(conn, id) else {
        return ResolvedContent::NotFound;
    };
    if item.project_id != project_id {
        return ResolvedContent::NotFound;
    }
    ResolvedContent::Item(ResolvedItem {
        sequence_id: item.sequence_id,
        name: item.name,
        state: state_name(conn, &item.state_id),
        priority: item.priority,
        assignee: item.assignee_agent,
    })
}

fn resolve_asset(conn: &Connection, project_id: &str, id: &str) -> ResolvedContent {
    let Ok(asset) = agentflare_backend::asset::get(conn, id) else {
        return ResolvedContent::NotFound;
    };
    if !asset_in_project(conn, &asset, project_id) {
        return ResolvedContent::NotFound;
    }
    let base_path = crate::paths::home().join(".agentflare");
    let content = agentflare_backend::asset::read_file(&base_path, &asset.storage_path)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|text| {
            if text.chars().count() > MAX_ASSET_CHARS {
                let mut truncated: String = text.chars().take(MAX_ASSET_CHARS).collect();
                truncated.push_str("… [truncated]");
                truncated
            } else {
                text
            }
        });
    ResolvedContent::Asset(ResolvedAsset {
        filename: asset.filename,
        entity_id: asset.entity_id,
        content,
    })
}

/// Assets attach to either an item or a project (`entity_type`) — resolve
/// the owning project either way and compare against the caller's.
fn asset_in_project(
    conn: &Connection,
    asset: &agentflare_backend::asset::Asset,
    project_id: &str,
) -> bool {
    match asset.entity_type.as_str() {
        "item_attachment" => agentflare_backend::item::get(conn, &asset.entity_id)
            .map(|item| item.project_id == project_id)
            .unwrap_or(false),
        "project_attachment" => asset.entity_id == project_id,
        _ => false,
    }
}

fn resolve_search(conn: &Connection, project_id: &str, query: &str) -> ResolvedContent {
    let items = agentflare_backend::item::search(conn, project_id, query, Some(SEARCH_LIMIT))
        .unwrap_or_default();
    ResolvedContent::Search(
        items
            .into_iter()
            .map(|item| SearchHit {
                sequence_id: item.sequence_id,
                state: state_name(conn, &item.state_id),
                name: item.name,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;
    use agentflare_backend::{asset, item, project, state, workspace};

    fn seed_project(conn: &Connection) -> (String, String) {
        let ws = workspace::create(
            conn,
            workspace::CreateWorkspace {
                name: "ws".into(),
                slug: "ws".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let proj = project::create(
            conn,
            project::CreateProject {
                workspace_id: ws.id,
                name: "proj".into(),
                identifier: "proj".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        state::seed_defaults(conn, &proj.id).unwrap();
        let sid = state::list_by_project(conn, &proj.id)
            .unwrap()
            .into_iter()
            .find(|s| s.is_default)
            .unwrap()
            .id;
        (proj.id, sid)
    }

    #[test]
    fn resolves_existing_item_by_id() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, state_id) = seed_project(&conn);
        let created = item::create(
            &conn,
            item::CreateItem {
                project_id,
                state_id,
                name: "Fix login timeout".into(),
                description: None,
                priority: Some("high".into()),
                parent_id: None,
                assignee_agent: Some("claude-code".into()),
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap();

        let mentions = vec![Mention {
            kind: MentionKind::Item,
            value: created.id.clone(),
        }];
        let resolved = resolve_mentions(&conn, &created.project_id, &mentions);
        match &resolved[0].content {
            ResolvedContent::Item(i) => {
                assert_eq!(i.name, "Fix login timeout");
                assert_eq!(i.assignee.as_deref(), Some("claude-code"));
            }
            _ => panic!("expected resolved item"),
        }
    }

    #[test]
    fn missing_item_resolves_to_not_found() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let mentions = vec![Mention {
            kind: MentionKind::Item,
            value: "does-not-exist".into(),
        }];
        let resolved = resolve_mentions(&conn, "any-project", &mentions);
        assert!(matches!(resolved[0].content, ResolvedContent::NotFound));
    }

    #[test]
    fn resolves_asset_content_and_truncates_long_files() {
        with_temp_home(|| {
            let conn = agentflare_backend::db::open_in_memory().unwrap();
            let (project_id, state_id) = seed_project(&conn);
            let item = item::create(
                &conn,
                item::CreateItem {
                    project_id: project_id.clone(),
                    state_id,
                    name: "with asset".into(),
                    description: None,
                    priority: None,
                    parent_id: None,
                    assignee_agent: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: None,
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                },
            )
            .unwrap();

            let base_path = crate::paths::home().join(".agentflare");
            let big_content = "x".repeat(5000);
            asset::write_file(&base_path, "assets/big.txt", big_content.as_bytes()).unwrap();
            let created = asset::create(
                &conn,
                asset::CreateAsset {
                    workspace_id: None,
                    entity_type: "item_attachment".into(),
                    entity_id: item.id.clone(),
                    filename: "big.txt".into(),
                    size: big_content.len() as i64,
                    mime_type: Some("text/plain".into()),
                    metadata: None,
                    storage_path: Some("assets/big.txt".into()),
                },
            )
            .unwrap();

            let mentions = vec![Mention {
                kind: MentionKind::Asset,
                value: created.id.clone(),
            }];
            let resolved = resolve_mentions(&conn, &project_id, &mentions);
            match &resolved[0].content {
                ResolvedContent::Asset(a) => {
                    assert_eq!(a.filename, "big.txt");
                    let content = a.content.as_ref().unwrap();
                    assert!(content.ends_with("… [truncated]"));
                    assert!(
                        content.chars().count()
                            <= MAX_ASSET_CHARS + "… [truncated]".chars().count()
                    );
                }
                _ => panic!("expected resolved asset"),
            }
        });
    }

    #[test]
    fn search_finds_matching_items_in_project() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, state_id) = seed_project(&conn);
        item::create(
            &conn,
            item::CreateItem {
                project_id: project_id.clone(),
                state_id,
                name: "Fix login timeout bug".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap();

        let mentions = vec![Mention {
            kind: MentionKind::Search,
            value: "login".into(),
        }];
        let resolved = resolve_mentions(&conn, &project_id, &mentions);
        match &resolved[0].content {
            ResolvedContent::Search(hits) => {
                assert_eq!(hits.len(), 1);
                assert_eq!(hits[0].name, "Fix login timeout bug");
            }
            _ => panic!("expected search results"),
        }
    }

    #[test]
    fn item_mention_is_scoped_to_project() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_a, state_a) = seed_project(&conn);
        let ws_b = workspace::create(
            &conn,
            workspace::CreateWorkspace {
                name: "ws2".into(),
                slug: "ws2".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let project_b = project::create(
            &conn,
            project::CreateProject {
                workspace_id: ws_b.id,
                name: "proj2".into(),
                identifier: "proj2".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap()
        .id;
        let item = item::create(
            &conn,
            item::CreateItem {
                project_id: project_a,
                state_id: state_a,
                name: "Only visible in project A".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap();

        let mentions = vec![Mention {
            kind: MentionKind::Item,
            value: item.id.clone(),
        }];
        let resolved = resolve_mentions(&conn, &project_b, &mentions);
        assert!(matches!(resolved[0].content, ResolvedContent::NotFound));
    }

    #[test]
    fn asset_mention_is_scoped_to_project() {
        with_temp_home(|| {
            let conn = agentflare_backend::db::open_in_memory().unwrap();
            let (project_a, state_a) = seed_project(&conn);
            let ws_b = workspace::create(
                &conn,
                workspace::CreateWorkspace {
                    name: "ws2".into(),
                    slug: "ws2".into(),
                    owner_agent: None,
                    item_label: None,
                },
            )
            .unwrap();
            let project_b = project::create(
                &conn,
                project::CreateProject {
                    workspace_id: ws_b.id,
                    name: "proj2".into(),
                    identifier: "proj2".into(),
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap()
            .id;
            let item = item::create(
                &conn,
                item::CreateItem {
                    project_id: project_a,
                    state_id: state_a,
                    name: "with asset".into(),
                    description: None,
                    priority: None,
                    parent_id: None,
                    assignee_agent: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: None,
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                },
            )
            .unwrap();

            let base_path = crate::paths::home().join(".agentflare");
            asset::write_file(&base_path, "assets/secret.txt", b"secret").unwrap();
            let created = asset::create(
                &conn,
                asset::CreateAsset {
                    workspace_id: None,
                    entity_type: "item_attachment".into(),
                    entity_id: item.id.clone(),
                    filename: "secret.txt".into(),
                    size: 6,
                    mime_type: Some("text/plain".into()),
                    metadata: None,
                    storage_path: Some("assets/secret.txt".into()),
                },
            )
            .unwrap();

            let mentions = vec![Mention {
                kind: MentionKind::Asset,
                value: created.id.clone(),
            }];
            let resolved = resolve_mentions(&conn, &project_b, &mentions);
            assert!(matches!(resolved[0].content, ResolvedContent::NotFound));
        });
    }
}
