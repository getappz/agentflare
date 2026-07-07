---
name: coderabbit-fix
description: Process CodeRabbit PR review comments systematically. Fetch comments via gh CLI, categorize by severity, plan minimal Rust fixes, implement, validate with cargo check/test/clippy, and document.
---

# CodeRabbit Fix Flow for agentflare

## Overview

Systematically process CodeRabbit review comments on agentflare PRs. Fetches all review comments, categorizes by type and severity, implements minimal fixes following ponytail principles, and validates with Rust toolchain.

## When to Use

After receiving CodeRabbit review on any agentflare PR, or when user says "fix coderabbit comments", "process coderabbit review", "address PR review".

## Workflow

### Step 1: Fetch Review Comments

Use GitHub CLI to get all review comments on the PR:

```bash
gh pr view <PR_NUMBER> --repo getappz/agentflare --json reviews --jq '.reviews[].body'
```

If PR number not provided, detect from current branch:
```bash
gh pr list --repo getappz/agentflare --head "$(git branch --show-current)" --json number --jq '.[0].number'
```

### Step 2: Categorize and Prioritize

Parse each comment by:

| Tag | Severity | Action |
|-----|----------|--------|
| Critical / Bug | Must fix | Fix immediately |
| Important | Should fix | Fix if root cause is real |
| Nitpick / Trivial | Optional | Fix only if quick win (< 5 min) |
| Performance | Optional | Fix if measurable impact |

Ignore:
- Comments about pre-existing code (outside PR diff)
- Comments on docs/plans/specs (non-runtime)
- Comments suggesting new features (scope creep)

### Step 3: Create Fix Todo List

Create `todowrite` tracking each fix. One item per actionable comment. Mark as `completed` after each fix verified.

### Step 4: Implement Fixes

For each fix:
1. Read the relevant file(s)
2. Apply minimal code change — ponytail principle: smallest diff that fixes the root cause
3. Run `cargo check` to verify compile
4. Run `cargo test ponytail` if touching ponytail code
5. Mark todo complete

### Step 5: Commit and Push

```bash
git add -u
git commit -m "fix: address coderabbit review — <summary>"
git push
```

Commit per logical group (one commit for all related fixes, not one per comment).

## Rust-Specific Patterns

### Type Safety
```rust
// Before: unsafe unwrap
let config = serde_json::from_str(&data).unwrap();

// After: handle error
let config = serde_json::from_str(&data).unwrap_or_default();
```

### Error Handling
```rust
// Before: swallowing all errors
let _ = std::fs::remove_file(path);

// After: ignore only NotFound
if let Err(e) = std::fs::remove_file(path) {
    if e.kind() != std::io::ErrorKind::NotFound {
        log::warn!("failed to remove flag: {e}");
    }
}
```

### Clippy Fixes
```bash
cargo clippy --fix --allow-dirty  # auto-fix simple warnings
cargo clippy -- -D warnings       # verify clean
```

### Test Validation
```bash
cargo check                       # fast compile check
cargo test ponytail               # ponytail-specific tests
cargo test                        # full suite (if touching shared code)
cargo clippy -- -D warnings       # lint gate
```

## Ponytail Integration

When fixing review comments, ponytail mode applies:
- Fix root cause, not symptom
- One guard in shared function > guards in every caller
- Smallest diff that addresses the actual issue
- Skip cosmetic nitpicks unless they hide real bugs

## Resources

This skill requires:
- GitHub CLI (`gh`) authenticated and on PATH
- Rust toolchain (`cargo`, `rustc`, `clippy`)
- Access to `getappz/agentflare` repo
