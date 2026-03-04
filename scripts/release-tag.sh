#!/bin/bash

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CARGO_TOML="$REPO_ROOT/Cargo.toml"
CARGO_TOML_RELATIVE="Cargo.toml"
CARGO_LOCK="$REPO_ROOT/Cargo.lock"
CARGO_LOCK_RELATIVE="Cargo.lock"
CHANGELOG_PATH="$REPO_ROOT/CHANGELOG.md"
CHANGELOG_RELATIVE="CHANGELOG.md"

if [ ! -f "$CARGO_TOML" ]; then
  echo "Cargo.toml not found at $CARGO_TOML" >&2
  exit 1
fi

if ! git -C "$REPO_ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "Not inside a git repository: $REPO_ROOT" >&2
  exit 1
fi

resolve_github_repo() {
  if [ -n "${SPACEBOT_RELEASE_REPO:-}" ]; then
    printf "%s\n" "$SPACEBOT_RELEASE_REPO"
    return
  fi

  local origin_url
  origin_url="$(git -C "$REPO_ROOT" config --get remote.origin.url 2>/dev/null || true)"
  if [ -z "$origin_url" ]; then
    return
  fi

  python3 - "$origin_url" <<'PY'
import re
import sys

origin = sys.argv[1].strip()
match = re.search(r"github\.com[:/]([^/]+)/([^/.]+)(?:\.git)?$", origin)
if not match:
    raise SystemExit(0)

print(f"{match.group(1)}/{match.group(2)}")
PY
}

resolve_marketing_copy_path() {
  if [ "${SPACEBOT_SKIP_MARKETING_COPY:-0}" = "1" ]; then
    printf "%s\n" ""
    return
  fi

  local path
  path="${SPACEBOT_RELEASE_MARKETING_COPY_FILE:-}"

  if [ -z "$path" ]; then
    cat >&2 <<'EOF'
Release marketing copy is required.

Create a markdown file and rerun, for example:
  marketing_file="$(mktemp)"
  printf "<release story markdown>\n" > "$marketing_file"
  SPACEBOT_RELEASE_MARKETING_COPY_FILE="$marketing_file" cargo bump patch

Set SPACEBOT_SKIP_MARKETING_COPY=1 to bypass this requirement.
EOF
    return 1
  fi

  if [ ! -f "$path" ]; then
    echo "Marketing copy file not found: $path" >&2
    return 1
  fi

  if [ ! -s "$path" ]; then
    echo "Marketing copy file is empty: $path" >&2
    return 1
  fi

  printf "%s\n" "$path"
}

generate_release_notes_body() {
  local tag_name="$1"
  local previous_tag="$2"
  local output_file="$3"
  local repo_slug
  repo_slug="$(resolve_github_repo)"

  if [ -z "$repo_slug" ]; then
    echo "Unable to determine GitHub repo slug from origin. Set SPACEBOT_RELEASE_REPO=<owner/repo>." >&2
    return 1
  fi

  if ! command -v gh >/dev/null 2>&1; then
    echo "gh CLI is required to generate release notes for CHANGELOG.md." >&2
    return 1
  fi

  if ! gh auth status >/dev/null 2>&1; then
    echo "gh CLI is not authenticated. Run 'gh auth login' first." >&2
    return 1
  fi

  local notes_json
  notes_json="$(mktemp)"
  local target_commitish
  target_commitish="$(git -C "$REPO_ROOT" rev-parse HEAD)"

  if [ -n "$previous_tag" ]; then
    gh api -X POST "repos/$repo_slug/releases/generate-notes" \
      -f "tag_name=$tag_name" \
      -f "target_commitish=$target_commitish" \
      -f "previous_tag_name=$previous_tag" \
      > "$notes_json"
  else
    gh api -X POST "repos/$repo_slug/releases/generate-notes" \
      -f "tag_name=$tag_name" \
      -f "target_commitish=$target_commitish" \
      > "$notes_json"
  fi

  python3 - "$notes_json" "$output_file" <<'PY'
import json
import sys

json_path, output_path = sys.argv[1], sys.argv[2]
with open(json_path, "r", encoding="utf-8") as handle:
    payload = json.load(handle)

body = (payload.get("body") or "").replace("\r\n", "\n").strip()
if not body:
    body = "_No release notes generated._"

with open(output_path, "w", encoding="utf-8") as handle:
    handle.write(body + "\n")
PY

  rm -f "$notes_json"
}

upsert_changelog_release() {
  local changelog_path="$1"
  local tag_name="$2"
  local release_notes_path="$3"
  local marketing_copy_path="${4:-}"

  python3 - "$changelog_path" "$tag_name" "$release_notes_path" "$marketing_copy_path" <<'PY'
import re
import sys
from pathlib import Path

changelog_path, tag_name, notes_path, marketing_copy_path = (
    sys.argv[1],
    sys.argv[2],
    sys.argv[3],
    sys.argv[4],
)
path = Path(changelog_path)

if path.exists():
    content = path.read_text(encoding="utf-8")
else:
    content = "# Changelog\n\n"

if not content.startswith("# Changelog"):
    content = "# Changelog\n\n" + content.lstrip()

notes = Path(notes_path).read_text(encoding="utf-8").strip()

marketing_copy = ""
if marketing_copy_path:
    marketing_copy = Path(marketing_copy_path).read_text(encoding="utf-8").strip()

parts = [f"## {tag_name}", ""]
if marketing_copy:
    parts.extend(["### Release Story", "", marketing_copy, ""])
parts.extend([notes, ""])
entry = "\n".join(parts)

release_heading_pattern = re.compile(r"(?m)^##\s+v?\d+\.\d+\.\d+\s*$")
target_heading_pattern = re.compile(rf"(?m)^##\s+{re.escape(tag_name)}\s*$")

target_match = target_heading_pattern.search(content)
if target_match:
    start = target_match.start()
    next_match = release_heading_pattern.search(content, target_match.end())
    end = next_match.start() if next_match else len(content)
    updated = content[:start].rstrip() + "\n\n" + entry + content[end:].lstrip("\n")
else:
    first_release = release_heading_pattern.search(content)
    if first_release:
        prefix = content[:first_release.start()].rstrip()
        suffix = content[first_release.start():].lstrip("\n")
        updated = prefix + "\n\n" + entry + suffix
    else:
        updated = content.rstrip() + "\n\n" + entry

path.write_text(updated.rstrip() + "\n", encoding="utf-8")
PY
}

disallowed_changes=()
marketing_copy_allowed_relative=""
if [ -n "${SPACEBOT_RELEASE_MARKETING_COPY_FILE:-}" ]; then
  case "${SPACEBOT_RELEASE_MARKETING_COPY_FILE}" in
    "$REPO_ROOT"/*)
      marketing_copy_allowed_relative="${SPACEBOT_RELEASE_MARKETING_COPY_FILE#$REPO_ROOT/}"
      ;;
    /*)
      ;;
    *)
      if [ "${SPACEBOT_RELEASE_MARKETING_COPY_FILE#../}" = "${SPACEBOT_RELEASE_MARKETING_COPY_FILE}" ]; then
        marketing_copy_allowed_relative="${SPACEBOT_RELEASE_MARKETING_COPY_FILE#./}"
      fi
      ;;
  esac
fi

while IFS= read -r file; do
  if [ -z "$file" ]; then
    continue
  fi

  if [ "$file" != "$CARGO_TOML_RELATIVE" ] \
    && [ "$file" != "$CARGO_LOCK_RELATIVE" ] \
    && [ "$file" != "$CHANGELOG_RELATIVE" ] \
    && { [ -z "$marketing_copy_allowed_relative" ] || [ "$file" != "$marketing_copy_allowed_relative" ]; }
  then
    disallowed_changes+=("$file")
  fi
done < <(
  {
    git -C "$REPO_ROOT" diff --name-only
    git -C "$REPO_ROOT" diff --cached --name-only
    git -C "$REPO_ROOT" ls-files --others --exclude-standard
  } | sort -u
)

if [ "${#disallowed_changes[@]}" -gt 0 ]; then
  echo "Refusing to run release bump with unrelated working tree changes:" >&2
  for file in "${disallowed_changes[@]}"; do
    echo "  - $file" >&2
  done
  echo "Commit or stash these changes, then run cargo bump again." >&2
  exit 1
fi

bump_input="${1:-patch}"

package_name="$(python3 - "$CARGO_TOML" <<'PY'
import re
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as file:
    lines = file.readlines()

in_package = False
for line in lines:
    stripped = line.strip()
    if stripped == "[package]":
        in_package = True
        continue
    if in_package and stripped.startswith("[") and stripped != "[package]":
        break
    if in_package:
        match = re.match(r'^name\s*=\s*"([^"]+)"\s*$', stripped)
        if match:
            print(match.group(1))
            sys.exit(0)

raise SystemExit("Could not find [package] name in Cargo.toml")
PY
)"

current_version="$(python3 - "$CARGO_TOML" <<'PY'
import re
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as file:
    lines = file.readlines()

in_package = False
for line in lines:
    stripped = line.strip()
    if stripped == "[package]":
        in_package = True
        continue
    if in_package and stripped.startswith("[") and stripped != "[package]":
        break
    if in_package:
        match = re.match(r'^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"\s*$', stripped)
        if match:
            print(match.group(1))
            sys.exit(0)

raise SystemExit("Could not find [package] version in Cargo.toml")
PY
)"

IFS='.' read -r major minor patch <<<"$current_version"

case "$bump_input" in
  major)
    next_version="$((major + 1)).0.0"
    ;;
  minor)
    next_version="$major.$((minor + 1)).0"
    ;;
  patch)
    next_version="$major.$minor.$((patch + 1))"
    ;;
  *)
    if [[ "$bump_input" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
      next_version="$bump_input"
    else
      echo "Invalid version bump '$bump_input'" >&2
      echo "Usage: ./scripts/release-tag.sh [major|minor|patch|X.Y.Z]" >&2
      exit 1
    fi
    ;;
esac

if [ "$current_version" = "$next_version" ]; then
  echo "Current version already $current_version" >&2
  exit 1
fi

tag_name="v$next_version"

if git rev-parse -q --verify "refs/tags/$tag_name" >/dev/null; then
  echo "Tag $tag_name already exists" >&2
  exit 1
fi

previous_tag=""
if git rev-parse -q --verify "refs/tags/v$current_version" >/dev/null; then
  previous_tag="v$current_version"
fi

marketing_copy_path="$(resolve_marketing_copy_path)"

release_notes_file="$(mktemp)"
generate_release_notes_body "$tag_name" "$previous_tag" "$release_notes_file"
upsert_changelog_release "$CHANGELOG_PATH" "$tag_name" "$release_notes_file" "$marketing_copy_path"
rm -f "$release_notes_file"

python3 - "$CARGO_TOML" "$current_version" "$next_version" <<'PY'
import re
import sys

path, current_version, next_version = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path, "r", encoding="utf-8") as file:
    lines = file.readlines()

in_package = False
updated = False

for index, line in enumerate(lines):
    stripped = line.strip()
    if stripped == "[package]":
        in_package = True
        continue

    if in_package and stripped.startswith("[") and stripped != "[package]":
        break

    if in_package and re.match(r'^version\s*=\s*"[0-9]+\.[0-9]+\.[0-9]+"\s*$', stripped):
        lines[index] = re.sub(
            r'^version\s*=\s*"[0-9]+\.[0-9]+\.[0-9]+"\s*$',
            f'version = "{next_version}"',
            stripped,
        ) + "\n"
        updated = True
        break

if not updated:
    raise SystemExit("Failed to update [package] version in Cargo.toml")

with open(path, "w", encoding="utf-8") as file:
    file.writelines(lines)
PY

if [ -f "$CARGO_LOCK" ]; then
  python3 - "$CARGO_LOCK" "$package_name" "$next_version" <<'PY'
import re
import sys

path, package_name, next_version = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path, "r", encoding="utf-8") as file:
    lines = file.readlines()

in_package = False
matched_package = False
updated = False

for index, line in enumerate(lines):
    stripped = line.strip()

    if stripped == "[[package]]":
        in_package = True
        matched_package = False
        continue

    if in_package and stripped.startswith("name = "):
        matched_package = stripped == f'name = "{package_name}"'
        continue

    if in_package and matched_package and stripped.startswith("version = "):
        lines[index] = re.sub(
            r'^version\s*=\s*"[0-9]+\.[0-9]+\.[0-9]+"\s*$',
            f'version = "{next_version}"',
            stripped,
        ) + "\n"
        updated = True
        break

if not updated:
    raise SystemExit(f'Failed to update package version for {package_name} in Cargo.lock')

with open(path, "w", encoding="utf-8") as file:
    file.writelines(lines)
PY
  git -C "$REPO_ROOT" add "$CARGO_TOML_RELATIVE" "$CARGO_LOCK_RELATIVE" "$CHANGELOG_RELATIVE"
else
  git -C "$REPO_ROOT" add "$CARGO_TOML_RELATIVE" "$CHANGELOG_RELATIVE"
fi

git -C "$REPO_ROOT" commit -m "release: $tag_name"
git -C "$REPO_ROOT" tag "$tag_name"

echo "Bumped Cargo.toml version: $current_version -> $next_version"
echo "Updated changelog entry: $CHANGELOG_RELATIVE ($tag_name)"
if [ -n "$marketing_copy_path" ]; then
  echo "Included release story from: $marketing_copy_path"
fi
echo "Created commit: release: $tag_name"
echo "Created tag: $tag_name"
echo "Next: git push && git push origin $tag_name"
