#!/usr/bin/env bash
set -euo pipefail

CARGO_TOML="Cargo.toml"
CURRENT_VERSION=$(grep '^version' "$CARGO_TOML" | head -1 | sed 's/.*"\(.*\)"/\1/')

usage() {
    echo "Usage: $0 <major|minor|patch|VERSION>"
    echo ""
    echo "Current version: $CURRENT_VERSION"
    echo ""
    echo "Examples:"
    echo "  $0 patch        # $CURRENT_VERSION -> next patch"
    echo "  $0 minor        # $CURRENT_VERSION -> next minor"
    echo "  $0 major        # $CURRENT_VERSION -> next major"
    echo "  $0 1.2.3        # set explicit version"
    exit 1
}

[ $# -eq 1 ] || usage

bump_version() {
    local current="$1" part="$2"
    IFS='.' read -r major minor patch <<< "$current"

    case "$part" in
        major) echo "$((major + 1)).0.0" ;;
        minor) echo "$major.$((minor + 1)).0" ;;
        patch) echo "$major.$minor.$((patch + 1))" ;;
        *) echo "$part" ;;
    esac
}

NEW_VERSION=$(bump_version "$CURRENT_VERSION" "$1")

if ! [[ "$NEW_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Error: invalid version format '$NEW_VERSION' (expected X.Y.Z)"
    exit 1
fi

echo "Bumping version: $CURRENT_VERSION -> $NEW_VERSION"

sed -i "s/^version = \"$CURRENT_VERSION\"/version = \"$NEW_VERSION\"/" "$CARGO_TOML"

cargo check --quiet 2>/dev/null || { echo "Error: cargo check failed"; exit 1; }

git add "$CARGO_TOML"
git commit -m "release: v$NEW_VERSION"
git tag -a "v$NEW_VERSION" -m "v$NEW_VERSION"

echo ""
echo "Done! Version bumped to v$NEW_VERSION"
echo "Run 'git push && git push --tags' to publish the release."
