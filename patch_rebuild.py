import re

with open('/home/avihs/projects/agentflare/.worktrees/task/185/crates/skill-registry/src/db.rs', 'r') as f:
    content = f.read()

# Update rebuild function to include category
old_rebuild = '''pub fn rebuild(conn: &mut Connection, entries: &[SkillEntry]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM skills", [])?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO skills (name, source, path, shadow_path, description, tags, est_tokens, compressed, last_used_at, bandit_alpha, bandit_beta, install_hint, remote_url)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        for e in entries {
            stmt.execute(params![
                e.name,
                e.source,
                e.path.to_string_lossy(),
                e.shadow_path.as_ref().map(|p| p.to_string_lossy().to_string()),
                e.description,
                e.tags,
                e.est_tokens,
                e.compressed as i64,
                0i64,
                1.0f64,
                1.0f64,
                e.install_hint,
                e.remote_url,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}'''

new_rebuild = '''pub fn rebuild(conn: &mut Connection, entries: &[SkillEntry]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM skills", [])?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO skills (name, source, path, shadow_path, description, tags, est_tokens, compressed, last_used_at, bandit_alpha, bandit_beta, install_hint, remote_url, category)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )?;
        for e in entries {
            stmt.execute(params![
                e.name,
                e.source,
                e.path.to_string_lossy(),
                e.shadow_path.as_ref().map(|p| p.to_string_lossy().to_string()),
                e.description,
                e.tags,
                e.est_tokens,
                e.compressed as i64,
                0i64,
                1.0f64,
                1.0f64,
                e.install_hint,
                e.remote_url,
                e.category,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}'''

content = content.replace(old_rebuild, new_rebuild)

with open('/home/avihs/projects/agentflare/.worktrees/task/185/crates/skill-registry/src/db.rs', 'w') as f:
    f.write(content)

print("rebuild function updated")