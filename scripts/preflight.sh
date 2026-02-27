#!/usr/bin/env bash
set -euo pipefail

usage() {
	cat <<'EOF'
Usage: scripts/preflight.sh [--ci]

Options:
  --ci   Run CI-safe checks (skip interactive auth checks).
EOF
}

is_ci=false

while (($# > 0)); do
	case "$1" in
	--ci)
		is_ci=true
		shift
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		echo "[preflight] ERROR: unknown argument: $1" >&2
		usage >&2
		exit 2
		;;
	esac
done

log() {
	echo "[preflight] $*"
}

fail() {
	echo "[preflight] ERROR: $*" >&2
	exit 1
}

require_command() {
	local command_name="$1"
	if ! command -v "$command_name" >/dev/null 2>&1; then
		fail "missing required command: $command_name"
	fi
}

require_command git
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || fail "not inside a git worktree"

repository_root="$(git rev-parse --show-toplevel)"
cd "$repository_root"

log "checking required commands"
require_command cargo
require_command rustc
require_command protoc

log "checking git state"
git remote get-url origin >/dev/null 2>&1 || fail "origin remote is not configured"

if [[ -n "$(git diff --name-only --diff-filter=U)" ]]; then
	fail "unmerged files detected; resolve conflicts before continuing"
fi

merge_head_path="$(git rev-parse --git-path MERGE_HEAD)"
rebase_merge_path="$(git rev-parse --git-path rebase-merge)"
rebase_apply_path="$(git rev-parse --git-path rebase-apply)"
cherry_pick_head_path="$(git rev-parse --git-path CHERRY_PICK_HEAD)"

if [[ -f "$merge_head_path" || -d "$rebase_merge_path" || -d "$rebase_apply_path" || -f "$cherry_pick_head_path" ]]; then
	fail "merge/rebase/cherry-pick in progress; finish it before running gates"
fi

current_branch="$(git rev-parse --abbrev-ref HEAD)"
if [[ "$current_branch" == "HEAD" && "$is_ci" == false ]]; then
	fail "detached HEAD is not supported for PR gate checks"
fi

if ! $is_ci; then
	log "checking remote access"
	if command -v gh >/dev/null 2>&1 && ! gh auth status -h github.com >/dev/null 2>&1; then
		log "gh is installed but not authenticated for github.com (continuing with git remote auth check)"
	fi

	if ! git ls-remote --heads origin >/dev/null 2>&1; then
		fail "cannot reach origin; verify network and git auth before pushing"
	fi
fi

log "preflight checks passed"
