//! Skills compiled into the agentflare binary itself, materialized to disk
//! and folded into `default_sources` unconditionally. Unlike `claude-user`/
//! `claude-project` (filesystem convention Claude Code owns), these are
//! always present — any cwd, any MCP client, no `.claude/skills` required.

use std::path::Path;

struct BuiltinSkill {
    name: &'static str,
    skill_md: &'static str,
    companions: &'static [(&'static str, &'static str)],
}

const BUILTIN_SKILLS: &[BuiltinSkill] = &[BuiltinSkill {
    name: "pm",
    skill_md: include_str!("../../../.claude/skills/pm/SKILL.md"),
    companions: &[
        (
            "reference/read-recipe.md",
            include_str!("../../../.claude/skills/pm/reference/read-recipe.md"),
        ),
        (
            "reference/rubric.md",
            include_str!("../../../.claude/skills/pm/reference/rubric.md"),
        ),
    ],
}];

fn write_if_changed(path: &Path, content: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|s| s == content) {
        return Ok(());
    }
    std::fs::write(path, content)
}

/// Write every `BUILTIN_SKILLS` entry under `dir` (one `<name>/SKILL.md` +
/// companions each), skipping unchanged files. Idempotent, safe to call on
/// every `ensure_fresh`.
pub fn materialize(dir: &Path) -> std::io::Result<()> {
    for skill in BUILTIN_SKILLS {
        let skill_dir = dir.join(skill.name);
        std::fs::create_dir_all(&skill_dir)?;
        write_if_changed(&skill_dir.join("SKILL.md"), skill.skill_md)?;
        for (rel, content) in skill.companions {
            let path = skill_dir.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            write_if_changed(&path, content)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materialize_writes_pm_skill_and_companions() {
        let tmp = tempfile::tempdir().unwrap();
        materialize(tmp.path()).unwrap();
        let skill_md = tmp.path().join("pm").join("SKILL.md");
        assert!(skill_md.is_file());
        let body = std::fs::read_to_string(&skill_md).unwrap();
        assert!(body.contains("name: pm"));
        assert!(
            tmp.path()
                .join("pm")
                .join("reference")
                .join("read-recipe.md")
                .is_file()
        );
        assert!(
            tmp.path()
                .join("pm")
                .join("reference")
                .join("rubric.md")
                .is_file()
        );
    }

    #[test]
    fn materialize_is_idempotent_and_skips_unchanged_writes() {
        let tmp = tempfile::tempdir().unwrap();
        materialize(tmp.path()).unwrap();
        let skill_md = tmp.path().join("pm").join("SKILL.md");
        let mtime_before = std::fs::metadata(&skill_md).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        materialize(tmp.path()).unwrap();
        let mtime_after = std::fs::metadata(&skill_md).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after);
    }
}
