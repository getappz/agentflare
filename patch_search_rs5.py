with open('/home/avihs/projects/agentflare/.worktrees/task/185/crates/skill-registry/src/search.rs', 'r') as f:
    content = f.read()

# Find the position after list_all_name_source_pairs function
idx = content.find("rows.collect()\n}\n\n#[cfg(test)]")
if idx >= 0:
    insert_pos = idx + len("rows.collect()\n}")
    new_func = '''

pub fn list_categories(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT category FROM skills WHERE category IS NOT NULL AND category != '' ORDER BY category")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    let mut categories = Vec::new();
    for row in rows {
        categories.push(row?);
    }
    Ok(categories)
}'''
    content = content[:insert_pos] + new_func + content[insert_pos:]
    with open('/home/avihs/projects/agentflare/.worktrees/task/185/crates/skill-registry/src/search.rs', 'w') as f:
        f.write(content)
    print("Function added successfully")
else:
    print("Marker not found")
    # Try alternative
    idx2 = content.find("rows.collect()\n}\n")
    if idx2 >= 0:
        print(f"Found at {idx2}: {repr(content[idx2:idx2+50])}")
    else:
        print("Not found at all")