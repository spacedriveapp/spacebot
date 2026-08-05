#!/usr/bin/env bash
#
# Fail if the frontend hand-writes a type the OpenAPI schema already defines.
#
# `check-typegen` only proves `schema.d.ts` matches the Rust. It says nothing
# about whether the app actually *uses* those types. For a long time it did not:
# `client.ts` declared its own copies of 114 response and request shapes, so the
# generated file could be perfectly in sync while the app compiled against
# something else entirely. Nothing caught the difference, because from the type
# checker's point of view there was no difference to catch — just two unrelated
# types that happened to share a name.
#
# What that cost, found when the duplicates were finally removed:
#   - the config API never exposed `*_thinking_effort`, so the dashboard's
#     Thinking Effort control read and wrote nothing
#   - `POST /tasks` ignored the `status` the dashboard sent, so every task
#     created from the UI came back awaiting approval
#   - 13 response types were missing server fields the UI could not reach
#   - nullable fields were typed as `string | undefined` while the server sends
#     `string | null`
#
# So: response and request types come from `schema.d.ts` via `types.ts`. Types
# with no server counterpart — SSE events, view models, component props — stay
# hand-written, and this check ignores them because they share no name with a
# schema type.

set -euo pipefail

cd "$(dirname "$0")/.."

SCHEMA="interface/src/api/schema.d.ts"
CLIENT="interface/src/api/client.ts"

for f in "$SCHEMA" "$CLIENT"; do
	if [[ ! -f "$f" ]]; then
		echo "check-api-types: missing $f" >&2
		exit 1
	fi
done

python3 - "$SCHEMA" "$CLIENT" <<'PY'
import re
import sys

schema_path, client_path = sys.argv[1], sys.argv[2]

with open(schema_path) as fh:
    schema = fh.read()
with open(client_path) as fh:
    client = fh.read()

# Component schemas are emitted at a fixed indent inside `components.schemas`.
schema_names = set(re.findall(r"^        ([A-Za-z_]\w*):", schema, re.M))

offenders = []
for match in re.finditer(r"^export (interface|type) ([A-Za-z_]\w*)\b", client, re.M):
    kind, name = match.group(1), match.group(2)
    if name not in schema_names:
        continue
    # An alias pointing at the generated schema is the whole point — allow it.
    tail = client[match.start():match.start() + 200]
    if re.match(r"^export type \w+ = (Types\.\w+|components\[)", tail):
        continue
    line = client[:match.start()].count("\n") + 1
    offenders.append((line, kind, name))

if offenders:
    print(
        "check-api-types: these are declared by hand in client.ts but already "
        "exist in the generated schema:\n",
        file=sys.stderr,
    )
    for line, kind, name in offenders:
        print(f"  client.ts:{line}  export {kind} {name}", file=sys.stderr)
    print(
        "\nReplace each with an alias:\n"
        "  export type <Name> = Types.<Name>;\n"
        "adding `export type <Name> = components[\"schemas\"][\"<Name>\"];` to "
        "api/types.ts if it is not there yet.\n"
        "\nIf the shapes genuinely differ, the server is the source of truth — "
        "fix the Rust type, not the TypeScript copy.",
        file=sys.stderr,
    )
    sys.exit(1)

print(f"check-api-types: OK ({len(schema_names)} schema types, no hand-written duplicates)")
PY
