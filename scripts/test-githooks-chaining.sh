#!/usr/bin/env bash
# Shell-level checks for .githooks chaining + sequencer skip. Run from repo root.
set -u
src="$(cd "$(dirname "$0")/.." && pwd)/.githooks"
fail=0
ok() { echo "ok   $1"; }
bad() { echo "FAIL $1"; fail=1; }

new_repo() {
    d="$(mktemp -d)"; cd "$d" || exit 1
    git init -q -b main; git config init.defaultBranch main; git config user.email t@t; git config user.name t
    mkdir .githooks; cp "$src"/* .githooks/; git config core.hooksPath .githooks
    git commit -q --allow-empty -m base --no-verify
    git checkout -q -b feat
}
local_hook() { # name body
    printf '#!/bin/sh\n%s\n' "$2" > ".git/hooks/$1"; chmod +x ".git/hooks/$1"
}

# 1. local pre-commit runs, and its failure blocks the commit
new_repo; local_hook pre-commit 'echo ran >> "$(git rev-parse --git-dir)/pc.log"; exit 1'
echo a > a; git add a
git commit -q -m x 2>/dev/null && bad "failing local pre-commit did not block" || ok "failing local pre-commit blocks"
[ -f .git/pc.log ] && ok "local pre-commit ran" || bad "local pre-commit did not run"

# 2. local post-commit runs
new_repo; local_hook post-commit 'touch "$(git rev-parse --git-dir)/pcm.ran"'
git commit -q --allow-empty -m y; [ -f .git/pcm.ran ] && ok "local post-commit ran" || bad "local post-commit"

# 3. prepare-commit-msg: local runs; stamping skipped under MERGE_HEAD etc.
new_repo; local_hook prepare-commit-msg 'echo local >> "$(git rev-parse --git-dir)/pcm.log"'
# fake agentflare: `git trailer-inject FILE` appends STAMPED; anything else no-ops
fake="$(mktemp -d)"
printf '#!/bin/sh\n[ "$2" = trailer-inject ] && echo STAMPED >> "$3"\nexit 0\n' > "$fake/agentflare"
chmod +x "$fake/agentflare"; export PATH="$fake:$PATH"
printf 'msg\n' > m; .githooks/prepare-commit-msg m
grep -q STAMPED m && ok "stamps normally" || bad "no stamp normally"
[ -f .git/pcm.log ] && ok "local prepare-commit-msg ran" || bad "local prepare-commit-msg"
for marker in MERGE_HEAD CHERRY_PICK_HEAD; do
    printf 'msg\n' > m; touch ".git/$marker"; .githooks/prepare-commit-msg m
    grep -q STAMPED m && bad "stamped during $marker" || ok "skip on $marker"; rm ".git/$marker"
done
for dir in rebase-apply rebase-merge; do
    printf 'msg\n' > m; mkdir ".git/$dir"; .githooks/prepare-commit-msg m
    grep -q STAMPED m && bad "stamped during $dir" || ok "skip on $dir"; rmdir ".git/$dir"
done

# 4. pre-push: stdin replayed to local hook and to our loop; local failure blocks
new_repo; local_hook pre-push 'cat > "$(git rev-parse --git-dir)/pp.in"; exit 3'
echo "refs/heads/feat abc refs/heads/feat def" | .githooks/pre-push origin url; rc=$?
[ "$rc" = 3 ] && ok "local pre-push failure propagates" || bad "pre-push rc=$rc"
grep -q "refs/heads/feat" .git/pp.in && ok "pre-push stdin replayed to local" || bad "pre-push stdin"
new_repo
echo "refs/heads/main abc refs/heads/main def" | .githooks/pre-push origin url 2>/dev/null
[ $? = 1 ] && ok "own pre-push loop still sees stdin" || bad "own pre-push loop lost stdin"

# 5. reference-transaction: local veto only in prepared; stdin replayed
new_repo; local_hook reference-transaction 'cat > "$(git rev-parse --git-dir)/rt.in"; exit 4'
echo "0 1 refs/heads/z" | .githooks/reference-transaction prepared; rc=$?
[ "$rc" = 4 ] && ok "prepared veto propagates" || bad "prepared rc=$rc"
grep -q refs/heads/z .git/rt.in && ok "ref-transaction stdin replayed" || bad "rt stdin"
echo "0 1 refs/heads/z" | .githooks/reference-transaction committed; rc=$?
[ "$rc" = 0 ] && ok "committed failure ignored" || bad "committed rc=$rc"
echo "0 1 refs/heads/z" | .githooks/reference-transaction preparing; rc=$?
[ "$rc" = 4 ] && ok "preparing veto propagates" || bad "preparing rc=$rc"

# 6. pre-merge-commit: runs local pre-merge-commit, then local pre-commit (a custom merge hook may not)
new_repo; local_hook pre-merge-commit 'echo mm >> "$(git rev-parse --git-dir)/mm.log"'
local_hook pre-commit 'echo pc >> "$(git rev-parse --git-dir)/pc2.log"'
.githooks/pre-merge-commit
[ -f .git/mm.log ] && ok "local pre-merge-commit ran" || bad "local pre-merge-commit"
[ -f .git/pc2.log ] && ok "local pre-commit veto applies to merges" || bad "local pre-commit skipped on merge"

exit $fail
