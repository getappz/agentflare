#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
tmp=$(mktemp -d "$PWD/.cleanup-branches-test.XXXXXX")
trap '[[ "$tmp" == "$PWD"/.cleanup-branches-test.* ]] && rm -rf -- "$tmp"' EXIT

git init --bare --quiet "$tmp/origin.git"
git clone --quiet "$tmp/origin.git" "$tmp/repo"
git -C "$tmp/repo" config user.name Test
git -C "$tmp/repo" config user.email test@example.invalid
git -C "$tmp/repo" commit --quiet --allow-empty -m initial
git -C "$tmp/repo" push --quiet -u origin HEAD:master
git -C "$tmp/repo" worktree add --quiet -b merged "$tmp/wt" HEAD
mkdir "$tmp/repo/scripts"
cp scripts/cleanup-branches.sh "$tmp/repo/scripts/cleanup-branches.sh"

echo local > "$tmp/wt/local.txt"
bash "$tmp/repo/scripts/cleanup-branches.sh" > "$tmp/dirty.out" 2>&1
git -C "$tmp/repo" show-ref --verify --quiet refs/heads/merged
git -C "$tmp/repo" worktree list --porcelain | grep -Fq 'branch refs/heads/merged'
test -f "$tmp/wt/local.txt"

rm "$tmp/wt/local.txt"
bash "$tmp/repo/scripts/cleanup-branches.sh" > "$tmp/clean.out" 2>&1
! git -C "$tmp/repo" show-ref --verify --quiet refs/heads/merged
test ! -e "$tmp/wt"
echo 'cleanup-branches: dirty worktree preserved; clean worktree removed'
