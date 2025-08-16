fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

readme:
    #!/usr/bin/env bash
    set -euo pipefail

    # Extract rustdoc comments from lib.rs and convert to markdown
    echo "Generating README.md from rustdoc..."

    # Use cargo doc to generate docs, then extract the main module doc
    cargo doc --no-deps --document-private-items --quiet

    # Extract the rustdoc content and convert it to README format
    sed -n '/^\/\/!/p' src/lib.rs | \
    sed 's/^\/\/! \?//' | \
    sed 's/^\/\/!$//' > README.md

    echo "README.md generated successfully!"

clippy:
    cargo clippy --all-targets --all-features -- -D warnings

checks:
    just fmt-check
    just clippy
    cargo test

# Version bump, build, commit, and tag
# Usage: just release [patch|minor|major]
release bump_type="patch":
    #!/usr/bin/env bash
    set -euo pipefail

    echo "🚀 Releasing new {{bump_type}} version..."

    # Check if cargo-edit is installed
    if ! command -v cargo-set-version &> /dev/null; then
        echo "❌ cargo-edit is not installed. Installing..."
        cargo install cargo-edit
    fi

    # Get current version
    current_version=$(cargo pkgid | cut -d# -f2 | cut -d: -f2)
    echo "📦 Current version: $current_version"

    # Bump version using cargo-edit
    echo "⬆️  Bumping {{bump_type}} version..."
    cargo set-version --bump {{bump_type}}

    # Get new version
    new_version=$(cargo pkgid | cut -d# -f2 | cut -d: -f2)
    echo "📦 New version: $new_version"

    # Run checks to ensure everything still works
    echo "🔍 Running checks..."
    just checks

    # Build release version
    echo "🔨 Building release..."
    cargo build --release

    # Add all changes
    git add .

    # Create commit with auto-generated message
    echo "💾 Committing changes..."
    git commit -m "Bump version to $new_version

    🤖 Generated with [Claude Code](https://claude.ai/code)

    Co-Authored-By: Claude <noreply@anthropic.com>"

    # Create git tag
    echo "🏷️  Creating tag v$new_version..."
    git tag "v$new_version"

    echo "✅ Release v$new_version ready!"
    echo "📤 To publish, run:"
    echo "   git push origin master"
    echo "   git push origin v$new_version"
    echo "   cargo publish"

# Full release with automatic push and publish
# Usage: just publish [patch|minor|major]
publish bump_type="patch":
    #!/usr/bin/env bash
    set -euo pipefail

    # Run the release process
    just release {{bump_type}}

    # Get the new version for confirmation
    new_version=$(cargo pkgid | cut -d# -f2 | cut -d: -f2)

    echo "🌐 Publishing release v$new_version..."

    # Push commits and tags
    echo "📤 Pushing to remote..."
    git push origin master
    git push origin "v$new_version"

    # Publish to crates.io
    echo "📦 Publishing to crates.io..."
    cargo publish

    echo "🎉 Release v$new_version published successfully!"
