#!/usr/bin/env bash
set -euo pipefail

usage() {
	cat <<'EOF'
Usage: scripts/gate-pr.sh [--ci] [--fast]

Options:
  --ci    CI mode (enables CI-oriented migration base resolution).
  --fast  Skip clippy and integration test compile for quicker local iteration.
EOF
}

is_ci=false
fast_mode=false

while (($# > 0)); do
	case "$1" in
	--ci)
		is_ci=true
		shift
		;;
	--fast)
		fast_mode=true
		shift
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		echo "[gate-pr] ERROR: unknown argument: $1" >&2
		usage >&2
		exit 2
		;;
	esac
done

log() {
	echo "[gate-pr] $*"
}

fail() {
	echo "[gate-pr] ERROR: $*" >&2
	exit 1
}

run_step() {
	local label="$1"
	shift
	log "running: $label"
	"$@"
}

resolve_migration_diff_range() {
	local base_ref=""
	local head_sha=""
	head_sha="$(git rev-parse HEAD)"

	if $is_ci \
		&& [[ -n "${GITHUB_EVENT_BEFORE:-}" && "${GITHUB_EVENT_BEFORE}" != "0000000000000000000000000000000000000000" ]] \
		&& git rev-parse --verify --quiet "${GITHUB_EVENT_BEFORE}^{commit}" >/dev/null 2>&1; then
		echo "${GITHUB_EVENT_BEFORE}..HEAD"
		return
	fi

	if [[ -n "${PR_GATE_BASE_REF:-}" ]]; then
		base_ref="${PR_GATE_BASE_REF}"
	elif [[ -n "${GITHUB_BASE_REF:-}" ]]; then
		base_ref="origin/${GITHUB_BASE_REF}"
	elif git symbolic-ref --quiet --short refs/remotes/origin/HEAD >/dev/null 2>&1; then
		base_ref="$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD)"
	elif git rev-parse --verify --quiet origin/main >/dev/null 2>&1; then
		base_ref="origin/main"
	fi

	if [[ -n "$base_ref" ]] && git rev-parse --verify --quiet "$base_ref" >/dev/null 2>&1; then
		local merge_base=""
		merge_base="$(git merge-base HEAD "$base_ref" 2>/dev/null || true)"
		if [[ -n "$merge_base" && "$merge_base" != "$head_sha" ]]; then
			echo "${merge_base}..HEAD"
			return
		fi
	fi

	if git rev-parse --verify --quiet HEAD~1 >/dev/null 2>&1; then
		echo "HEAD~1..HEAD"
	fi
}

check_migration_safety() {
	log "checking migration safety"

	local diff_range=""
	diff_range="$(resolve_migration_diff_range)"
	if [[ -n "$diff_range" ]]; then
		log "migration diff range: $diff_range"
	else
		log "migration diff range: working tree only (no base ref available)"
	fi

	local -a migration_changes=()
	local migration_change=""
	while IFS= read -r migration_change; do
		migration_changes+=("$migration_change")
	done < <(
		{
			if [[ -n "$diff_range" ]]; then
				git diff --name-status "$diff_range" -- migrations
			fi
			git diff --name-status --cached -- migrations
			git diff --name-status -- migrations
			git ls-files --others --exclude-standard -- migrations | sed $'s/^/A\t/'
		} | sed '/^[[:space:]]*$/d' | sort -u
	)

	if ((${#migration_changes[@]} == 0)); then
		log "migration safety passed (no migration changes detected)"
		return
	fi

	declare -A paths_with_add=()
	for line in "${migration_changes[@]}"; do
		local status=""
		local path=""
		if [[ "$line" == *$'\t'* ]]; then
			status="${line%%$'\t'*}"
			path="${line#*$'\t'}"
		else
			status="${line%% *}"
			path="${line#* }"
			[[ "$path" == "$line" ]] && path=""
		fi
		if [[ -n "$path" && "$status" == A* ]]; then
			paths_with_add["$path"]=1
		fi
	done

	local violations=()
	for line in "${migration_changes[@]}"; do
		local status=""
		local path=""
		if [[ "$line" == *$'\t'* ]]; then
			status="${line%%$'\t'*}"
			path="${line#*$'\t'}"
		else
			status="${line%% *}"
			path="${line#* }"
			[[ "$path" == "$line" ]] && path=""
		fi
		if [[ -n "$path" && "$status" != A* && -z "${paths_with_add[$path]:-}" ]]; then
			violations+=("$line")
		fi
	done

	if ((${#violations[@]} > 0)); then
		echo "[gate-pr] ERROR: existing migration files were modified:" >&2
		printf '  %s\n' "${violations[@]}" >&2
		fail "create a new timestamped migration instead of editing migration history"
	fi

	log "migration safety passed (only new migration files detected)"
}

repository_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[[ -n "$repository_root" ]] || fail "not inside a git worktree"
cd "$repository_root"

if $is_ci; then
	log "CI mode enabled"
fi

check_migration_safety
run_step "cargo fmt --all -- --check" cargo fmt --all -- --check
run_step "cargo check --all-targets" cargo check --all-targets

if $fast_mode; then
	log "fast mode enabled: skipping clippy and integration test compile"
else
	run_step "cargo clippy --all-targets" cargo clippy --all-targets
fi

run_step "cargo test --lib" cargo test --lib

if ! $fast_mode; then
	run_step "cargo test --tests --no-run" cargo test --tests --no-run
fi

log "all gate checks passed"
