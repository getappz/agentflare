import re

with open('/home/avihs/projects/agentflare/.worktrees/task/185/src/mcp_server/skill.rs', 'r') as f:
    content = f.read()

# Add categories case before the default case
old_match_end = '''            "create" => {
            }
                format!("unknown action: {other}"),
        }'''

new_match_end = '''            "create" => {
            }
            "categories" => {
                let categories = self.with_backend_db(|conn| {
                    skill_registry::search::list_categories(conn)
                })?;
                Ok(serde_json::json!({ "categories": categories }).to_string())
            }
                format!("unknown action: {other}"),
        }'''

content = content.replace(old_match_end, new_match_end)

with open('/home/avihs/projects/agentflare/.worktrees/task/185/src/mcp_server/skill.rs', 'w') as f:
    f.write(content)

print("categories action added to skill.rs")