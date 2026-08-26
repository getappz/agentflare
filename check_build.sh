#!/bin/bash
cd /home/avihs/projects/agentflare/.worktrees/task/185
cargo check -p agentflare-skill-registry 2>&1 | head -50