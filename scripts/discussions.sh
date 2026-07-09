#!/usr/bin/env bash
set -euo pipefail

# gh api graphql wrapper for GitHub Discussions — gh CLI has no native
# `gh discussion` subcommand and Discussions live behind GraphQL only
# (no REST endpoint). Field names below are verified against GitHub's
# live GraphQL schema via introspection, not memory.

REPO_OWNER="${REPO_OWNER:-getappz}"
REPO_NAME="${REPO_NAME:-agentflare}"

error() {
	echo "Error: $*" >&2
	exit 1
}

usage() {
	cat <<EOF
Usage: $(basename "$0") <command> [args]

Commands:
  categories                                   List categories (slug, emoji, name, description)
  list [category-slug]                         List discussions, optionally filtered by category
  get <number>                                  Show id/title/body/url/locked/category
  create <category-slug> <title> <body-file>   Create a discussion
  update <number> [--title T] [--body-file F] [--category slug]
  comment <number> <body-file>                  Add a comment
  lock <number>                                  Lock (GitHub rejects a lockReason for Discussions specifically)
  unlock <number>                               Unlock
  answer <comment-node-id>                      Mark a comment as the answer
  unanswer <comment-node-id>                     Unmark a comment as the answer
  delete <number>                               Delete (requires CONFIRM=yes env var)

Env: REPO_OWNER (default getappz), REPO_NAME (default agentflare)
EOF
	exit 1
}

repo_id() {
	gh api graphql -f query='
		query($owner: String!, $name: String!) {
			repository(owner: $owner, name: $name) { id }
		}' -f owner="$REPO_OWNER" -f name="$REPO_NAME" --jq '.data.repository.id'
}

discussion_id() {
	local number="$1"
	gh api graphql -f query='
		query($owner: String!, $name: String!, $number: Int!) {
			repository(owner: $owner, name: $name) {
				discussion(number: $number) { id }
			}
		}' -f owner="$REPO_OWNER" -f name="$REPO_NAME" -F number="$number" \
		--jq '.data.repository.discussion.id // empty'
}

category_id() {
	local slug="$1"
	gh api graphql -f query='
		query($owner: String!, $name: String!) {
			repository(owner: $owner, name: $name) {
				discussionCategories(first: 25) { nodes { id slug } }
			}
		}' -f owner="$REPO_OWNER" -f name="$REPO_NAME" \
		--jq ".data.repository.discussionCategories.nodes[] | select(.slug==\"$slug\") | .id"
}

cmd_categories() {
	gh api graphql -f query='
		query($owner: String!, $name: String!) {
			repository(owner: $owner, name: $name) {
				discussionCategories(first: 25) {
					nodes { slug emoji name description isAnswerable }
				}
			}
		}' -f owner="$REPO_OWNER" -f name="$REPO_NAME" \
		--jq '.data.repository.discussionCategories.nodes[] | "\(.slug)\t\(.emoji)\t\(.name)\t\(.description)"'
}

cmd_list() {
	local slug="${1:-}"
	if [ -n "$slug" ]; then
		local cid
		cid="$(category_id "$slug")"
		[ -n "$cid" ] || error "unknown category slug: $slug (run: $(basename "$0") categories)"
		gh api graphql -f query='
			query($owner: String!, $name: String!, $cid: ID!) {
				repository(owner: $owner, name: $name) {
					discussions(first: 50, categoryId: $cid) {
						nodes { number title url category { slug } }
					}
				}
			}' -f owner="$REPO_OWNER" -f name="$REPO_NAME" -f cid="$cid" \
			--jq '.data.repository.discussions.nodes[] | "\(.number)\t\(.category.slug)\t\(.title)"'
	else
		gh api graphql -f query='
			query($owner: String!, $name: String!) {
				repository(owner: $owner, name: $name) {
					discussions(first: 50) {
						nodes { number title url category { slug } }
					}
				}
			}' -f owner="$REPO_OWNER" -f name="$REPO_NAME" \
			--jq '.data.repository.discussions.nodes[] | "\(.number)\t\(.category.slug)\t\(.title)"'
	fi
}

cmd_get() {
	local number="${1:?discussion number required}"
	gh api graphql -f query='
		query($owner: String!, $name: String!, $number: Int!) {
			repository(owner: $owner, name: $name) {
				discussion(number: $number) {
					id title body url locked category { slug }
				}
			}
		}' -f owner="$REPO_OWNER" -f name="$REPO_NAME" -F number="$number" \
		--jq '.data.repository.discussion'
}

cmd_create() {
	local slug="${1:?category slug required}"
	local title="${2:?title required}"
	local body_file="${3:?body file required}"
	[ -f "$body_file" ] || error "body file not found: $body_file"

	local rid cid
	rid="$(repo_id)"
	cid="$(category_id "$slug")"
	[ -n "$cid" ] || error "unknown category slug: $slug (run: $(basename "$0") categories)"

	gh api graphql -f query='
		mutation($repoId: ID!, $catId: ID!, $title: String!, $body: String!) {
			createDiscussion(input: {repositoryId: $repoId, categoryId: $catId, title: $title, body: $body}) {
				discussion { number url }
			}
		}' -f repoId="$rid" -f catId="$cid" -f title="$title" -F body="@$body_file" \
		--jq '.data.createDiscussion.discussion'
}

cmd_update() {
	local number="${1:?discussion number required}"
	shift
	local did
	did="$(discussion_id "$number")"
	[ -n "$did" ] || error "discussion #$number not found"

	local title="" body_file="" slug="" cid=""
	while [ $# -gt 0 ]; do
		case "$1" in
		--title)
			title="$2"
			shift 2
			;;
		--body-file)
			body_file="$2"
			shift 2
			;;
		--category)
			slug="$2"
			shift 2
			;;
		*) error "unknown flag: $1" ;;
		esac
	done
	[ -n "$title" ] || [ -n "$body_file" ] || [ -n "$slug" ] || error "nothing to update — pass --title/--body-file/--category"

	# GraphQL variables must be declared with their types up front, so the
	# query text is assembled from only the fields actually requested —
	# this keeps an unset field genuinely absent from the mutation input
	# rather than sending it as an explicit null (which would clear it).
	local decl="\$id: ID!" fields="discussionId: \$id"
	local args=(-f "id=$did")

	if [ -n "$title" ]; then
		decl="$decl, \$title: String!"
		fields="$fields, title: \$title"
		args+=(-f "title=$title")
	fi
	if [ -n "$body_file" ]; then
		[ -f "$body_file" ] || error "body file not found: $body_file"
		decl="$decl, \$body: String!"
		fields="$fields, body: \$body"
		args+=(-F "body=@$body_file")
	fi
	if [ -n "$slug" ]; then
		cid="$(category_id "$slug")"
		[ -n "$cid" ] || error "unknown category slug: $slug (run: $(basename "$0") categories)"
		decl="$decl, \$catId: ID!"
		fields="$fields, categoryId: \$catId"
		args+=(-f "catId=$cid")
	fi

	gh api graphql -f query="mutation($decl) { updateDiscussion(input: {$fields}) { discussion { number url } } }" \
		"${args[@]}" --jq '.data.updateDiscussion.discussion'
}

cmd_comment() {
	local number="${1:?discussion number required}"
	local body_file="${2:?body file required}"
	[ -f "$body_file" ] || error "body file not found: $body_file"
	local did
	did="$(discussion_id "$number")"
	[ -n "$did" ] || error "discussion #$number not found"

	gh api graphql -f query='
		mutation($id: ID!, $body: String!) {
			addDiscussionComment(input: {discussionId: $id, body: $body}) {
				comment { id url }
			}
		}' -f id="$did" -F body="@$body_file" --jq '.data.addDiscussionComment.comment'
}

cmd_lock() {
	local number="${1:?discussion number required}"
	local did
	did="$(discussion_id "$number")"
	[ -n "$did" ] || error "discussion #$number not found"

	# lockReason is a valid LockLockableInput field for Issues/PRs, but the
	# API rejects it for Discussions specifically ("You cannot specify a
	# reason when locking a discussion") — confirmed by testing, not docs.
	gh api graphql -f query='
		mutation($id: ID!) {
			lockLockable(input: {lockableId: $id}) {
				lockedRecord { locked }
			}
		}' -f id="$did" --jq '.data.lockLockable.lockedRecord'
}

cmd_unlock() {
	local number="${1:?discussion number required}"
	local did
	did="$(discussion_id "$number")"
	[ -n "$did" ] || error "discussion #$number not found"

	gh api graphql -f query='
		mutation($id: ID!) {
			unlockLockable(input: {lockableId: $id}) {
				unlockedRecord { locked }
			}
		}' -f id="$did" --jq '.data.unlockLockable.unlockedRecord'
}

cmd_answer() {
	local comment_id="${1:?comment node id required}"
	gh api graphql -f query='
		mutation($id: ID!) {
			markDiscussionCommentAsAnswer(input: {id: $id}) {
				discussion { number }
			}
		}' -f id="$comment_id" --jq '.data.markDiscussionCommentAsAnswer.discussion'
}

cmd_unanswer() {
	local comment_id="${1:?comment node id required}"
	gh api graphql -f query='
		mutation($id: ID!) {
			unmarkDiscussionCommentAsAnswer(input: {id: $id}) {
				discussion { number }
			}
		}' -f id="$comment_id" --jq '.data.unmarkDiscussionCommentAsAnswer.discussion'
}

cmd_delete() {
	local number="${1:?discussion number required}"
	[ "${CONFIRM:-}" = "yes" ] || error "refusing to delete without confirmation — rerun as: CONFIRM=yes $(basename "$0") delete $number"
	local did
	did="$(discussion_id "$number")"
	[ -n "$did" ] || error "discussion #$number not found"

	gh api graphql -f query='
		mutation($id: ID!) {
			deleteDiscussion(input: {id: $id}) {
				discussion { number }
			}
		}' -f id="$did" --jq '.data.deleteDiscussion.discussion'
}

[ $# -ge 1 ] || usage
cmd="$1"
shift
case "$cmd" in
categories) cmd_categories "$@" ;;
list) cmd_list "$@" ;;
get) cmd_get "$@" ;;
create) cmd_create "$@" ;;
update) cmd_update "$@" ;;
comment) cmd_comment "$@" ;;
lock) cmd_lock "$@" ;;
unlock) cmd_unlock "$@" ;;
answer) cmd_answer "$@" ;;
unanswer) cmd_unanswer "$@" ;;
delete) cmd_delete "$@" ;;
*) usage ;;
esac
