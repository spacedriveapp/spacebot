#!/bin/sh
# Build the unified spacebot+spacedrive Docker image locally.
#
# By default, fetches spacedrive at the SHA pinned in `.spacedrive-version`
# and spaceui at `main` from GitHub, and builds the image tagged
# `spacebot-unified:local`.
#
# Usage:
#   scripts/build-unified.sh                # fetch pinned refs from GitHub
#   scripts/build-unified.sh --local        # use sibling worktrees at
#                                           # ../spacedrive and ../spaceui
#                                           # (captures uncommitted changes)
#   scripts/build-unified.sh --pull         # re-fetch even if .build/ exists
#   scripts/build-unified.sh --tag NAME     # custom image tag
#   scripts/build-unified.sh --local --tag spacebot:dev
#
# The build context is always the spacebot repo root. CI uses the same
# Dockerfile with .build/ populated by actions/checkout steps.

set -eu

# cd to spacebot repo root regardless of where the script was invoked from.
cd "$(dirname "$0")/.."

TAG="spacebot-unified:local"
USE_LOCAL=0
PULL=0

while [ $# -gt 0 ]; do
    case "$1" in
        --local)
            USE_LOCAL=1
            shift
            ;;
        --pull)
            PULL=1
            shift
            ;;
        --tag)
            if [ $# -lt 2 ]; then
                echo "--tag requires an argument" >&2
                exit 1
            fi
            TAG="$2"
            shift 2
            ;;
        -h|--help)
            sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

if [ ! -f .spacedrive-version ]; then
    echo "Missing .spacedrive-version at repo root" >&2
    exit 1
fi

SD_REF=$(tr -d '[:space:]' < .spacedrive-version)
if [ -z "$SD_REF" ]; then
    echo ".spacedrive-version is empty" >&2
    exit 1
fi

mkdir -p .build

if [ "$USE_LOCAL" = "1" ]; then
    # Sync from sibling worktrees — captures whatever's on disk, including
    # uncommitted changes. Useful when iterating on spacedrive or spaceui
    # alongside spacebot and you want the next Docker build to pick up
    # your local edits without pushing them first.
    if [ ! -d ../spacedrive ]; then
        echo "Expected sibling checkout at ../spacedrive — clone it or drop --local" >&2
        exit 1
    fi
    if [ ! -d ../spaceui ]; then
        echo "Expected sibling checkout at ../spaceui — clone it or drop --local" >&2
        exit 1
    fi

    if ! command -v rsync >/dev/null 2>&1; then
        echo "rsync is required for --local; install it and retry" >&2
        exit 1
    fi

    echo "Syncing ../spacedrive -> .build/spacedrive (excluding build artifacts)"
    rsync -a --delete \
        --exclude .git \
        --exclude target \
        --exclude node_modules \
        --exclude "apps/web/dist" \
        --exclude "apps/*/dist" \
        ../spacedrive/ .build/spacedrive/

    echo "Syncing ../spaceui -> .build/spaceui (excluding build artifacts)"
    rsync -a --delete \
        --exclude .git \
        --exclude node_modules \
        --exclude dist \
        ../spaceui/ .build/spaceui/
else
    # Fetch pinned refs from GitHub. Re-clone if --pull, or if the target
    # directories aren't git checkouts yet.
    if [ "$PULL" = "1" ] || [ ! -d .build/spacedrive/.git ]; then
        echo "Cloning spacedrive @ $SD_REF"
        rm -rf .build/spacedrive
        git clone https://github.com/spacedriveapp/spacedrive.git .build/spacedrive
    else
        echo "Using existing .build/spacedrive (pass --pull to re-fetch)"
    fi
    (
        cd .build/spacedrive
        git fetch origin "$SD_REF"
        git checkout --detach "$SD_REF"
    )

    if [ "$PULL" = "1" ] || [ ! -d .build/spaceui/.git ]; then
        echo "Cloning spaceui @ main"
        rm -rf .build/spaceui
        git clone --depth 1 https://github.com/spacedriveapp/spaceui.git .build/spaceui
    else
        echo "Using existing .build/spaceui (pass --pull to re-fetch)"
    fi
fi

echo "Building Docker image: $TAG"
docker build -t "$TAG" .

cat <<EOF

Built: $TAG

Run:
    docker run --rm \\
        -p 19898:19898 \\
        -p 8080:8080 \\
        -v \$(pwd)/.docker-data:/data \\
        -e ANTHROPIC_API_KEY=... \\
        $TAG

Then open http://localhost:19898 and click the Files button in the sidebar footer.
EOF
