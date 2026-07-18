//! Orchestrates: read source, split frontmatter, sensitive-path refusal,
//! size cap, LLM call, empty/identical-output guards, backup (if in-place),
//! write target, validate-retry loop, restore-on-final-failure.

use crate::error::CavemanError;
use crate::frontmatter;
use crate::llm::Llm;
use crate::prompt::Prompt;
use crate::sensitive::is_sensitive_path;
use crate::validate::validate;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

const MAX_FILE_SIZE: u64 = 500_000; // 500KB
const MAX_RETRIES: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupMode {
    Sibling,
    OutOfTree,
}

#[derive(Debug)]
pub struct Report {
    pub original_bytes: usize,
    pub compressed_bytes: usize,
    pub original_path: std::path::PathBuf,
}

pub fn compress(
    llm: &dyn Llm,
    source: &Path,
    target: &Path,
    prompt: Prompt,
    backup: BackupMode,
) -> Result<Report, CavemanError> {
    if !source.is_file() {
        return Err(CavemanError::NotFound(source.display().to_string()));
    }
    let size = source.metadata()?.len();
    if size > MAX_FILE_SIZE {
        return Err(CavemanError::TooLarge(source.display().to_string()));
    }
    if is_sensitive_path(source) {
        return Err(CavemanError::Sensitive(source.display().to_string()));
    }

    let original_text = std::fs::read_to_string(source)?;
    if original_text.trim().is_empty() {
        return Err(CavemanError::Empty(source.display().to_string()));
    }

    let (frontmatter_text, body) = frontmatter::split(&original_text);
    if body.trim().is_empty() {
        return Err(CavemanError::Empty(format!(
            "{} (body after frontmatter removal)",
            source.display()
        )));
    }

    let in_place = source == target;
    let backup_path = backup_path_for(target, backup);
    if in_place && backup_path.exists() {
        return Err(CavemanError::BackupExists(
            backup_path.display().to_string(),
        ));
    }

    let compressed_body = llm.call(&prompt.build_compress_prompt(&body))?;
    if compressed_body.trim().is_empty() {
        return Err(CavemanError::EmptyResponse);
    }
    if compressed_body.trim() == body.trim() {
        return Err(CavemanError::IdenticalOutput);
    }

    let mut compressed = format!("{frontmatter_text}{compressed_body}");

    if in_place {
        if let Some(parent) = backup_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&backup_path, &original_text)?;
        let readback = std::fs::read_to_string(&backup_path)?;
        if readback != original_text {
            let _ = std::fs::remove_file(&backup_path);
            return Err(CavemanError::BackupVerifyFailed(
                backup_path.display().to_string(),
            ));
        }
    }

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(target, &compressed)?;

    let mut errors;
    for attempt in 0..MAX_RETRIES {
        let current = std::fs::read_to_string(target)?;
        errors = validate(&original_text, &current);
        if errors.is_empty() {
            break;
        }
        if attempt == MAX_RETRIES - 1 {
            if in_place {
                std::fs::write(source, &original_text)?;
                let _ = std::fs::remove_file(&backup_path);
            } else {
                let _ = std::fs::remove_file(target);
            }
            return Err(CavemanError::ValidationFailed(MAX_RETRIES, errors));
        }
        let fixed = llm.call(&prompt.build_fix_prompt(&original_text, &compressed, &errors))?;
        if fixed.trim().is_empty() {
            continue; // keep the prior attempt on disk, next loop iteration re-validates it
        }
        compressed = format!("{frontmatter_text}{fixed}");
        std::fs::write(target, &compressed)?;
    }

    let original_path = if in_place {
        backup_path
    } else {
        source.to_path_buf()
    };
    Ok(Report {
        original_bytes: original_text.len(),
        compressed_bytes: std::fs::metadata(target)?.len() as usize,
        original_path,
    })
}

fn out_of_tree_backup_dir(namespace: &str) -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("agentflare")
        .join(namespace)
        .join("backups")
}

fn backup_path_for(target: &Path, mode: BackupMode) -> PathBuf {
    match mode {
        BackupMode::Sibling => {
            let file_name = target
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            target.with_file_name(format!("{file_name}.orig"))
        }
        BackupMode::OutOfTree => {
            // Hash the full parent path, not just its last component — two
            // files with the same name under differently-located but
            // identically-named parent dirs (e.g. "project-a/docs/README.md"
            // and "project-b/docs/README.md") would otherwise collide on
            // the same backup path.
            let parent = target
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default();
            let canonical_parent = std::fs::canonicalize(&parent).unwrap_or(parent);
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            canonical_parent.hash(&mut hasher);
            let dir_hash = hasher.finish();
            let stem = target
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let sub = format!("{dir_hash:016x}");
            let file = format!("{stem}.original.md");

            let current = out_of_tree_backup_dir("flare-output")
                .join(&sub)
                .join(&file);
            // The backup namespace was renamed from "caveman" to
            // "flare-output". If a pre-rename backup already sits at the
            // old path, keep resolving to it — otherwise it (and the
            // "backup already exists" guard above, which checks this same
            // path) would silently stop seeing it.
            let legacy = out_of_tree_backup_dir("caveman").join(&sub).join(&file);
            if !current.exists() && legacy.exists() {
                legacy
            } else {
                current
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::FakeLlm;
    use tempfile::tempdir;

    fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn compresses_in_place_when_no_validation_errors() {
        let dir = tempdir().unwrap();
        let source = write(dir.path(), "doc.md", "# Title\n\nverbose text here");
        let llm = FakeLlm::queue(&["# Title\n\nterse"]);
        let report =
            compress(&llm, &source, &source, Prompt::Generic, BackupMode::Sibling).unwrap();
        assert_eq!(report.original_bytes, "# Title\n\nverbose text here".len());
        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            "# Title\n\nterse"
        );
        let backup = dir.path().join("doc.md.orig");
        assert!(backup.exists());
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "# Title\n\nverbose text here"
        );
    }

    #[test]
    fn refuses_sensitive_filename() {
        let dir = tempdir().unwrap();
        let source = write(dir.path(), "credentials.md", "some content");
        let llm = FakeLlm::queue(&["irrelevant"]);
        let err =
            compress(&llm, &source, &source, Prompt::Generic, BackupMode::Sibling).unwrap_err();
        assert!(matches!(err, CavemanError::Sensitive(_)), "{err:?}");
    }

    #[test]
    fn refuses_empty_file() {
        let dir = tempdir().unwrap();
        let source = write(dir.path(), "doc.md", "   \n  ");
        let llm = FakeLlm::queue(&["irrelevant"]);
        let err =
            compress(&llm, &source, &source, Prompt::Generic, BackupMode::Sibling).unwrap_err();
        assert!(matches!(err, CavemanError::Empty(_)), "{err:?}");
    }

    #[test]
    fn refuses_too_large_file() {
        let dir = tempdir().unwrap();
        let big = "x".repeat(600_000);
        let source = write(dir.path(), "doc.md", &big);
        let llm = FakeLlm::queue(&["irrelevant"]);
        let err =
            compress(&llm, &source, &source, Prompt::Generic, BackupMode::Sibling).unwrap_err();
        assert!(matches!(err, CavemanError::TooLarge(_)), "{err:?}");
    }

    #[test]
    fn refuses_when_backup_already_exists() {
        let dir = tempdir().unwrap();
        let source = write(dir.path(), "doc.md", "some real content");
        write(dir.path(), "doc.md.orig", "pre-existing backup");
        let llm = FakeLlm::queue(&["irrelevant"]);
        let err =
            compress(&llm, &source, &source, Prompt::Generic, BackupMode::Sibling).unwrap_err();
        assert!(matches!(err, CavemanError::BackupExists(_)), "{err:?}");
    }

    #[test]
    fn refuses_empty_llm_response() {
        let dir = tempdir().unwrap();
        let source = write(dir.path(), "doc.md", "real content here");
        let llm = FakeLlm::queue(&[""]);
        let err =
            compress(&llm, &source, &source, Prompt::Generic, BackupMode::Sibling).unwrap_err();
        assert!(matches!(err, CavemanError::EmptyResponse), "{err:?}");
    }

    #[test]
    fn refuses_identical_output() {
        let dir = tempdir().unwrap();
        let source = write(dir.path(), "doc.md", "unchanged text");
        let llm = FakeLlm::queue(&["unchanged text"]);
        let err =
            compress(&llm, &source, &source, Prompt::Generic, BackupMode::Sibling).unwrap_err();
        assert!(matches!(err, CavemanError::IdenticalOutput), "{err:?}");
    }

    #[test]
    fn retries_and_succeeds_after_a_fix() {
        let dir = tempdir().unwrap();
        let original = "# T\n\n```py\nprint(1)\n```\nverbose";
        let source = write(dir.path(), "doc.md", original);
        // First compress response drops the code block (fails validation),
        // second (fix) response restores it (passes).
        let llm = FakeLlm::queue(&[
            "# T\n\nterse, no code block",
            "# T\n\n```py\nprint(1)\n```\nterse",
        ]);
        let report =
            compress(&llm, &source, &source, Prompt::Generic, BackupMode::Sibling).unwrap();
        assert!(
            std::fs::read_to_string(&source)
                .unwrap()
                .contains("print(1)")
        );
        assert!(report.compressed_bytes > 0);
    }

    #[test]
    fn restores_original_after_exhausting_retries() {
        let dir = tempdir().unwrap();
        let original = "# T\n\n```py\nprint(1)\n```\nverbose";
        let source = write(dir.path(), "doc.md", original);
        // Every response drops the code block — validation never passes.
        let llm = FakeLlm::queue(&["missing code", "still missing code"]);
        let err =
            compress(&llm, &source, &source, Prompt::Generic, BackupMode::Sibling).unwrap_err();
        assert!(
            matches!(err, CavemanError::ValidationFailed(2, _)),
            "{err:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            original,
            "source must be restored"
        );
        assert!(
            !dir.path().join("doc.md.orig").exists(),
            "backup must be cleaned up"
        );
    }

    #[test]
    fn out_of_tree_backup_paths_dont_collide_for_same_named_parents() {
        let root = tempdir().unwrap();
        let a_dir = root.path().join("project-a").join("docs");
        let b_dir = root.path().join("project-b").join("docs");
        std::fs::create_dir_all(&a_dir).unwrap();
        std::fs::create_dir_all(&b_dir).unwrap();
        let a = write(&a_dir, "README.md", "a content");
        let b = write(&b_dir, "README.md", "b content");

        let backup_a = backup_path_for(&a, BackupMode::OutOfTree);
        let backup_b = backup_path_for(&b, BackupMode::OutOfTree);
        assert_ne!(
            backup_a, backup_b,
            "same-named parent dirs must not collide"
        );
    }

    #[test]
    fn out_of_tree_backup_falls_back_to_pre_rename_caveman_path() {
        let dir = tempdir().unwrap();
        let target = write(dir.path(), "legacy-fallback-test.md", "content");

        // Nothing backed up anywhere yet: resolves under the current
        // "flare-output" namespace.
        let resolved = backup_path_for(&target, BackupMode::OutOfTree);
        assert!(resolved.to_string_lossy().contains("flare-output"));
        assert!(!resolved.exists());

        // Simulate a backup left behind by a pre-rename version of this
        // tool, at the equivalent path under the old "caveman" namespace.
        let current_root = out_of_tree_backup_dir("flare-output");
        let relative = resolved.strip_prefix(&current_root).unwrap();
        let legacy = out_of_tree_backup_dir("caveman").join(relative);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "pre-rename backup").unwrap();

        let resolved_again = backup_path_for(&target, BackupMode::OutOfTree);
        assert_eq!(
            resolved_again, legacy,
            "must resolve to the existing legacy backup instead of orphaning it"
        );

        std::fs::remove_file(&legacy).unwrap();
    }

    #[test]
    fn shadow_copy_write_needs_no_backup() {
        let dir = tempdir().unwrap();
        let source = write(dir.path(), "source.md", "plugin content here");
        let target = dir.path().join("shadow.md");
        let llm = FakeLlm::queue(&["compressed content"]);
        compress(&llm, &source, &target, Prompt::Generic, BackupMode::Sibling).unwrap();
        assert!(!dir.path().join("shadow.md.orig").exists());
        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            "plugin content here",
            "source untouched"
        );
    }

    #[test]
    fn report_points_original_path_at_backup_for_in_place() {
        let dir = tempdir().unwrap();
        let src = write(
            dir.path(),
            "doc.md",
            "# Title\n\nlong original body worth compressing.\n",
        );
        let llm = FakeLlm::queue(&["# Title\n\nshort body"]);
        let report = compress(&llm, &src, &src, Prompt::Generic, BackupMode::Sibling).unwrap();
        assert_eq!(report.original_path, src.with_file_name("doc.md.orig"));
        assert!(report.original_path.is_file());
    }

    #[test]
    fn report_points_original_path_at_source_for_out_of_place() {
        let dir = tempdir().unwrap();
        let src = write(
            dir.path(),
            "doc.md",
            "# Title\n\nlong original body worth compressing.\n",
        );
        let tgt = dir.path().join("out.md");
        let llm = FakeLlm::queue(&["# Title\n\nshort body"]);
        let report = compress(&llm, &src, &tgt, Prompt::Generic, BackupMode::Sibling).unwrap();
        assert_eq!(report.original_path, src);
    }
}
