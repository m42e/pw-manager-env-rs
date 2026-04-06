#!/usr/bin/env bash
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "Usage: $0 <version>" >&2
  echo "Example: $0 1.0.1" >&2
  exit 1
fi

VERSION="$1"

# Validate semver format
if ! echo "$VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "Error: version must be in semver format (e.g. 1.2.3)" >&2
  exit 1
fi

TAG="v$VERSION"

# Ensure working tree is clean
if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "Error: working tree is not clean" >&2
  exit 1
fi

# Check tag does not already exist
if git rev-parse "$TAG" >/dev/null 2>&1; then
  echo "Error: tag $TAG already exists" >&2
  exit 1
fi

# Update version in Cargo.toml
sed -i '' "s/^version = \".*\"/version = \"$VERSION\"/" Cargo.toml
cargo check --quiet 2>/dev/null || cargo update -p pw-env

# Generate release notes
mkdir -p release-notes
if command -v git-cliff >/dev/null 2>&1; then
  git-cliff --config .github/cliff.toml --tag "$TAG" --unreleased --strip header \
    -o "release-notes/$TAG.md"
  echo "Generated release-notes/$TAG.md"
else
  echo "Warning: git-cliff not found, skipping release notes generation" >&2
fi

# Commit and tag
git add Cargo.toml Cargo.lock release-notes/
git commit -m "chore: prepare release $TAG"
git tag -a "$TAG" -m "Release $TAG"

echo ""
echo "Release $TAG prepared locally."
echo "Review the commit and tag, then push with:"
echo "  git push origin main $TAG"
