import re

with open('/home/avihs/projects/agentflare/.worktrees/task/185/crates/skill-registry/src/lib.rs', 'r') as f:
    content = f.read()

# Add list_categories to exports
old_export = 'pub use search::{MatchMode, SkillHit, merge_registry_hits, search};'
new_export = 'pub use search::{MatchMode, SkillHit, merge_registry_hits, search, list_categories};'

content = content.replace(old_export, new_export)

with open('/home/avihs/projects/agentflare/.worktrees/task/185/crates/skill-registry/src/lib.rs', 'w') as f:
    f.write(content)

print("list_categories exported")