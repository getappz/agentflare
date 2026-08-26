import re

with open('/home/avihs/projects/agentflare/.worktrees/task/185/crates/skill-registry/src/search.rs', 'r') as f:
    content = f.read()

# Add list_categories function after list_all_name_source_pairs
old_func = '''pub fn list_all_name_source_pairs(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
}
#[cfg(test)]'''

new_func = '''pub fn list_all_name_source_pairs(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
}

pub fn list_categories(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT category FROM skills WHERE category IS NOT NULL AND category != '' ORDER BY category")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    let mut categories = Vec::new();
    for row in rows {
        categories.push(row?);
    }
    Ok(categories)
}

#[cfg(test)]'''

content = content.replace(old_func, new_func)

with open('/home/avihs/projects/agentflare/.worktrees/task/185/crates/skill-registry/src/search.rs', 'w') as f:
    f.write(content)

print("list_categories function added")