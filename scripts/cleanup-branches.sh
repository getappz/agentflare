#!/usr/bin/env bash
# Delete local branches + worktrees already merged (or remote-deleted after
# squash-merge) into origin/<default>. --remote also prunes matching remote
# branches. Default: apply locally; remote deletion always needs --remote.
set -euo pipefail

cd "$(dirname "$0")/.."

DRY_RUN=0
DO_REMOTE=0
DEFAULT_BRANCH="${DEFAULT_BRANCH:-master}"
CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
# CURRENT_BRANCH guards against a fresh, never-committed branch: it has zero
# commits ahead of origin/master, so it looks "merged" by pure topology.
PROTECTED=("$DEFAULT_BRANCH" "main" "master" "$CURRENT_BRANCH")

for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    --remote) DO_REMOTE=1 ;;
    -h|--help)
      echo "Usage: $0 [--dry-run] [--remote]"
      echo "  --dry-run  list what would be removed, change nothing"
      echo "  --remote   also delete remote branches merged into origin/$DEFAULT_BRANCH"
      exit 0
      ;;
    *) echo "Unknown option: $arg" >&2; exit 1 ;;
  esac
done

is_protected() {
  local b=$1
  for p in "${PROTECTED[@]}"; do
    [[ "$b" == "$p" ]] && return 0
  done
  return 1
}

echo "== git fetch --prune =="
git fetch --prune --quiet origin

# branch -> worktree path, for branches currently checked out in a worktree
declare -A worktree_of
while IFS= read -r path && IFS= read -r branch; do
  branch=${branch#refs/heads/}
  [[ -n "$branch" ]] && worktree_of["$branch"]="$path"
done < <(git worktree list --porcelain | awk '
  /^worktree /{p=$2}
  /^branch /{print p; print $2}
')

# Merge candidates: branches whose upstream is gone (GitHub auto-deletes the
# remote branch on squash-merge, so this is the reliable signal for that
# flow) plus real ancestors of the default branch (fast-forward/regular
# merges, which never show "gone").
mapfile -t gone < <(git for-each-ref --format='%(refname:short) %(upstream:track)' refs/heads |
  awk '/\[gone\]/{print $1}')
mapfile -t merged < <(git branch --format='%(refname:short)' --merged "origin/$DEFAULT_BRANCH")

declare -A candidates
for b in "${gone[@]:-}" "${merged[@]:-}"; do
  [[ -n "$b" ]] && candidates["$b"]=1
done

removed=0
for b in "${!candidates[@]}"; do
  is_protected "$b" && continue

  wt="${worktree_of[$b]:-}"
  if ((DRY_RUN)); then
    if [[ -n "$wt" ]]; then
      echo "would remove worktree $wt + branch $b"
    else
      echo "would delete branch $b"
    fi
    continue
  fi

  # -D force-deletes without a merge check, so verify the branch's own
  # changes are actually present in origin/$DEFAULT_BRANCH BEFORE touching
  # its worktree or ref. A plain ancestry check (`git branch --merged`)
  # misses squash-merges (different tree shape), which is why "gone" is in
  # the candidate set at all — but that same gap means a branch with
  # unpushed commits beyond what was squash-merged would otherwise lose its
  # worktree checkout and ref silently. Checking first (not just before the
  # branch delete) means a "risky" branch keeps both intact.
  merge_base=$(git merge-base "origin/$DEFAULT_BRANCH" "$b" 2>/dev/null) || {
    echo "skip $b: no common history with origin/$DEFAULT_BRANCH" >&2
    continue
  }
  mapfile -t touched < <(git diff --name-only "$merge_base" "$b")
  if ((${#touched[@]})) && ! git diff --quiet "origin/$DEFAULT_BRANCH" "$b" -- "${touched[@]}"; then
    echo "skip $b: differs from origin/$DEFAULT_BRANCH in files it touched (possible unpushed work)" >&2
    continue
  fi

  if [[ -n "$wt" ]]; then
    if git worktree remove "$wt" 2>/dev/null; then
      echo "removed worktree $wt"
    else
      echo "skip $b: worktree $wt has uncommitted changes" >&2
      continue
    fi
  fi

  if git branch -D "$b" >/dev/null 2>&1; then
    echo "deleted branch $b"
    removed=$((removed + 1))
  fi
done

git worktree prune

if ((DO_REMOTE)); then
  echo "== remote branches merged into origin/$DEFAULT_BRANCH =="
  while IFS= read -r rb; do
    rb=${rb#origin/}
    [[ "$rb" == "HEAD" ]] && continue
    is_protected "$rb" && continue
    if ((DRY_RUN)); then
      echo "would delete remote branch origin/$rb"
    else
      git push origin --delete "$rb" && echo "deleted remote branch origin/$rb"
    fi
  done < <(git branch -r --format='%(refname:short)' --merged "origin/$DEFAULT_BRANCH")
fi

((DRY_RUN)) || echo "done: $removed local branch(es) removed"
