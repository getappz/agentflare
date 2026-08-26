import re

with open('/home/avihs/projects/agentflare/.worktrees/task/185/src/mcp_server/types.rs', 'r') as f:
    content = f.read()

# Update the action description to include categories
old_desc = '#[schemars(description = "Action: search|load|create")]'
new_desc = '#[schemars(description = "Action: search|load|create|categories")]'

content = content.replace(old_desc, new_desc)

with open('/home/avihs/projects/agentflare/.worktrees/task/185/src/mcp_server/types.rs', 'w') as f:
    f.write(content)

print("types.rs updated")