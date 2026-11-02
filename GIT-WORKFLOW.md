# Git shim workflow

For item work, use `item(action="claim", id=<item>)` and work in the returned
`worktree_path`. The standalone `claim` tool takes a scope lock only; it does not
create a checkout. A failed item claim includes `worktree_error`; do not start
editing the canonical checkout when worktree creation fails. New worktrees use
the active parent's branch, or the repository's resolved default branch.

`item(action="release", id=<item>)` attempts to remove a clean worktree owned by
the caller before releasing the claim. Dirty worktrees remain for recovery.
`preserve_worktree=true` keeps a clean worktree for later dispatch as well.

To return the canonical checkout to its default branch, first commit pending
changes on the feature branch or stash them with `git stash push -u -m <tag>`.
A checkout or switch to the default branch is allowed from a clean tree. Item
feature work still belongs in its claimed worktree; normal commit and push
policy checks still apply when landing changes.

Repository selectors (`-C`, `--git-dir`, `--work-tree`) allow supported read
commands, including `rev-parse`, `ls-files`, `branch --show-current`,
`branch --list`, `worktree list`, and `remote -v`. For mutations, change directory
into the target repository so the shim can classify it there.

A plain commit checks staged paths only. Commits that can include working-tree
content (`-a`, `--include`, interactive/patch mode, or pathspecs) also check
unstaged paths. Unknown options retain this broader check. Push scope checks
and trust-root diffs cap their changed-path listings at 50,000 paths; tooling
errors are reported separately from policy denials.

Bash's DEBUG guard blocks force pushes in tracked repositories before git runs.
Denials are recorded in `~/.agentflare/audit/guard.log` and explain the override.
After explicit user approval, start the retry shell with `AF_GUARD_OFF=1` in its
environment; prefer `--force-with-lease`. This disables that shell's Bash guard;
the git shim and repository hooks still apply. Shim decisions and explicit shim
bypasses are recorded separately in `~/.agentflare/audit/git.jsonl`.
