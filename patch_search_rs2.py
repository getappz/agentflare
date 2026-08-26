import re

with open('/home/avihs/projects/agentflare/.worktrees/task/185/crates/skill-registry/src/search.rs', 'r') as f:
    content = f.read()

# Find the exact location to insert the function
old_text = '''pub fn list_all_name_source_pairs(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
}
#[cfg(test)]'''

new_text = '''pub fn list_all_name_source_pairs(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
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

if old_text in content:
    content = content.replace(old_text, new_text)
    with open('/home/avihs/projects/agentflare/.worktrees/task/185/crates/skill-registry/src/search.rs', 'w') as f:
        f.write(content)
    print("Function added successfully")
else:
    print("Old text not found, trying alternative...")
    # Try with different whitespace
    alt_old = "pub fn list_all_name_source_pairs(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {\n}\n#[cfg(test)]"
    if alt_old in content:
        content = content.replace(alt_old, new_text)
        with open('/home/avihs/projects/agentflare/.worktrees/task/185/crates/skill-registry/src/search.rs', 'w') as f:
            f.write(content)
        print("Function added with alt")
    else:
        print("Could not find insertion point")
        # Print context around the function
        idx = content.find("list_all_name_source_pairs")
        if idx >= 0:
            print(repr(content[idx:idx+200]))