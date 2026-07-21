use crate::sources::SkillEntry;
use std::collections::HashSet;

/// Serializable skill bundle for import/export between registries.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillBundle {
    pub format_version: u32,
    pub entries: Vec<BundleEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BundleEntry {
    pub name: String,
    pub source: String,
    pub description: String,
    pub body: String,
    pub tags: String,
    pub est_tokens: i64,
}

impl SkillBundle {
    pub const FORMAT_VERSION: u32 = 1;

    /// Export a list of SkillEntry values into a BundleEntry list (strip
    /// fs-only fields like path, shadow_path, mtime).
    pub fn new(entries: &[SkillEntry]) -> Self {
        Self {
            format_version: Self::FORMAT_VERSION,
            entries: entries
                .iter()
                .map(|e| BundleEntry {
                    name: e.name.clone(),
                    source: e.source.clone(),
                    description: e.description.clone(),
                    body: e.body.clone(),
                    tags: e.tags.clone(),
                    est_tokens: e.est_tokens,
                })
                .collect(),
        }
    }

    /// Serialize to pretty JSON.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Deserialize from JSON string.
    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json)
    }

    /// Convert to SkillEntry values suitable for rebuilding the DB.
    /// Each entry gets a synthetic path (not used for file I/O).
    pub fn to_entries(&self, synthetic_base: &std::path::Path) -> Vec<SkillEntry> {
        self.entries
            .iter()
            .map(|be| SkillEntry {
                name: be.name.clone(),
                source: be.source.clone(),
                path: synthetic_base.join(&be.name).join("SKILL.md"),
                description: be.description.clone(),
                body: be.body.clone(),
                neg_text: String::new(),
                tags: be.tags.clone(),
                est_tokens: be.est_tokens,
                mtime: 0,
                shadow_path: None,
            })
            .collect()
    }

    /// Deduplicate entries: keep first occurrence of each (name, source) pair.
    /// Returns number of entries removed.
    pub fn dedup(&mut self) -> usize {
        let before = self.entries.len();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        self.entries
            .retain(|e| seen.insert((e.name.clone(), e.source.clone())));
        before - self.entries.len()
    }

    /// Merge another bundle into this one: append entries from `other` whose
    /// (name, source) keys are not already present. Returns count added.
    pub fn merge(&mut self, other: &SkillBundle) -> usize {
        let existing: HashSet<(String, String)> = self
            .entries
            .iter()
            .map(|e| (e.name.clone(), e.source.clone()))
            .collect();
        let mut added = 0;
        for e in &other.entries {
            let key = (e.name.clone(), e.source.clone());
            if !existing.contains(&key) {
                self.entries.push(e.clone());
                added += 1;
            }
        }
        added
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(name: &str, source: &str) -> SkillEntry {
        SkillEntry {
            name: name.into(),
            source: source.into(),
            path: std::path::PathBuf::new(),
            description: "desc".into(),
            body: "body".into(),
            neg_text: String::new(),
            tags: String::new(),
            est_tokens: 10,
            mtime: 0,
            shadow_path: None,
        }
    }

    #[test]
    fn roundtrip_json() {
        let entries = vec![sample_entry("test-skill", "local")];
        let bundle = SkillBundle::new(&entries);
        let json = bundle.to_json().unwrap();
        let restored = SkillBundle::from_json(&json).unwrap();
        assert_eq!(restored.entries.len(), 1);
        assert_eq!(restored.entries[0].name, "test-skill");
    }

    #[test]
    fn dedup_removes_duplicate_name_source() {
        let mut bundle = SkillBundle {
            format_version: 1,
            entries: vec![
                BundleEntry {
                    name: "a".into(),
                    source: "local".into(),
                    description: "first".into(),
                    body: "".into(),
                    tags: "".into(),
                    est_tokens: 0,
                },
                BundleEntry {
                    name: "a".into(),
                    source: "local".into(),
                    description: "second duplicate".into(),
                    body: "".into(),
                    tags: "".into(),
                    est_tokens: 0,
                },
            ],
        };
        assert_eq!(bundle.dedup(), 1);
        assert_eq!(bundle.entries[0].description, "first");
    }

    #[test]
    fn merge_adds_new_keys_only() {
        let mut bundle =
            SkillBundle::new(&[sample_entry("a", "local"), sample_entry("b", "local")]);
        let other = SkillBundle::new(&[sample_entry("b", "local"), sample_entry("c", "local")]);
        assert_eq!(bundle.merge(&other), 1);
        assert_eq!(bundle.entries.len(), 3);
    }

    #[test]
    fn to_entries_strips_fs_fields() {
        let bundle = SkillBundle::new(&[sample_entry("x", "hub")]);
        let entries = bundle.to_entries(std::path::Path::new("/tmp/skills"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "x");
        assert_eq!(entries[0].neg_text, "");
        assert_eq!(entries[0].mtime, 0);
    }
}
