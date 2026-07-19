use super::parse::MentionKind;
use super::resolve::{ResolvedContent, ResolvedMention};

pub fn format_context(resolved: &[ResolvedMention]) -> String {
    resolved
        .iter()
        .map(format_one)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn format_one(r: &ResolvedMention) -> String {
    let prefix = match r.kind {
        MentionKind::Item => "I",
        MentionKind::Asset => "A",
        MentionKind::Search => "search",
    };
    match &r.content {
        ResolvedContent::Item(i) => format!(
            "@{prefix}:{}: #{} {}\n  state: {}\n  priority: {}\n  assignee: {}",
            r.value,
            i.sequence_id,
            i.name,
            i.state,
            i.priority,
            i.assignee.as_deref().unwrap_or("unassigned"),
        ),
        ResolvedContent::Asset(a) => match &a.content {
            Some(text) => format!(
                "@{prefix}:{}: {}\n  (attached to {})\n  --- content ---\n  {}",
                r.value, a.filename, a.entity_id, text
            ),
            None => format!(
                "@{prefix}:{}: {}\n  (attached to {}, content not shown — binary or unreadable)",
                r.value, a.filename, a.entity_id
            ),
        },
        ResolvedContent::Search(hits) if hits.is_empty() => {
            format!("@{prefix}:{}: no matching items", r.value)
        }
        ResolvedContent::Search(hits) => {
            let lines: Vec<String> = hits
                .iter()
                .enumerate()
                .map(|(idx, h)| {
                    format!("  {}. #{} {} ({})", idx + 1, h.sequence_id, h.name, h.state)
                })
                .collect();
            format!("@{prefix}:{}:\n{}", r.value, lines.join("\n"))
        }
        ResolvedContent::NotFound => format!("@{prefix}:{}: not found", r.value),
    }
}

#[cfg(test)]
mod tests {
    use super::super::resolve::{ResolvedAsset, ResolvedItem, SearchHit};
    use super::*;

    #[test]
    fn formats_resolved_item() {
        let resolved = vec![ResolvedMention {
            kind: MentionKind::Item,
            value: "abc".into(),
            content: ResolvedContent::Item(ResolvedItem {
                sequence_id: 42,
                name: "Fix login timeout".into(),
                state: "started".into(),
                priority: "high".into(),
                assignee: Some("claude-code".into()),
            }),
        }];
        let out = format_context(&resolved);
        assert!(out.contains("@I:abc: #42 Fix login timeout"));
        assert!(out.contains("state: started"));
        assert!(out.contains("assignee: claude-code"));
    }

    #[test]
    fn formats_not_found() {
        let resolved = vec![ResolvedMention {
            kind: MentionKind::Asset,
            value: "missing".into(),
            content: ResolvedContent::NotFound,
        }];
        assert_eq!(format_context(&resolved), "@A:missing: not found");
    }

    #[test]
    fn formats_asset_with_and_without_content() {
        let resolved = vec![
            ResolvedMention {
                kind: MentionKind::Asset,
                value: "a1".into(),
                content: ResolvedContent::Asset(ResolvedAsset {
                    filename: "notes.txt".into(),
                    entity_id: "item-1".into(),
                    content: Some("hello".into()),
                }),
            },
            ResolvedMention {
                kind: MentionKind::Asset,
                value: "a2".into(),
                content: ResolvedContent::Asset(ResolvedAsset {
                    filename: "photo.png".into(),
                    entity_id: "item-2".into(),
                    content: None,
                }),
            },
        ];
        let out = format_context(&resolved);
        assert!(out.contains("--- content ---\n  hello"));
        assert!(out.contains("content not shown — binary or unreadable"));
    }

    #[test]
    fn formats_search_hits_and_empty_results() {
        let resolved = vec![ResolvedMention {
            kind: MentionKind::Search,
            value: "login".into(),
            content: ResolvedContent::Search(vec![SearchHit {
                sequence_id: 7,
                name: "Fix login timeout".into(),
                state: "started".into(),
            }]),
        }];
        assert!(format_context(&resolved).contains("1. #7 Fix login timeout (started)"));

        let empty = vec![ResolvedMention {
            kind: MentionKind::Search,
            value: "xyzzy".into(),
            content: ResolvedContent::Search(vec![]),
        }];
        assert_eq!(format_context(&empty), "@search:xyzzy: no matching items");
    }
}
