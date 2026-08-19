# Release a new version. Usage: just release <patch|minor|major>
release bump:
    #!/usr/bin/env bash
    set -euo pipefail

    BUMP="{{bump}}"
    case "$BUMP" in patch|minor|major) ;; *) echo "Usage: just release <patch|minor|major>" >&2; exit 1 ;; esac

    # Must be on master with clean tree
    [ "$(git branch --show-current)" = "master" ] || { echo "Error: not on master" >&2; exit 1; }
    git diff --quiet && git diff --cached --quiet || { echo "Error: dirty working tree" >&2; exit 1; }

    # Read current version, compute new
    CURRENT=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
    IFS='.' read -r MAJ MIN PAT <<< "$CURRENT"
    case "$BUMP" in
      patch) PAT=$((PAT + 1)) ;;
      minor) MIN=$((MIN + 1)); PAT=0 ;;
      major) MAJ=$((MAJ + 1)); MIN=0; PAT=0 ;;
    esac
    NEW="${MAJ}.${MIN}.${PAT}"

    echo "$CURRENT → $NEW"
    read -rp "Release v${NEW}? [y/N] " CONFIRM
    [[ "$CONFIRM" =~ ^[yY] ]] || { echo "Aborted."; exit 0; }

    # Bump version
    sed -i '' "s/^version = \"${CURRENT}\"/version = \"${NEW}\"/" Cargo.toml
    cargo check --quiet 2>/dev/null || cargo generate-lockfile

    # Update changelog
    git cliff --tag "v${NEW}" --output CHANGELOG.md

    # Commit and tag
    git add Cargo.toml Cargo.lock CHANGELOG.md
    git commit -m "chore(release): prepare for v${NEW}"
    git tag -a "v${NEW}" -m "Release v${NEW}"

    echo ""
    echo "Ready. Push with:"
    echo "  git push origin master && git push origin v${NEW}"

# Generate changelog without releasing
changelog:
    git cliff --output CHANGELOG.md

# Everything CI and the pre-commit hooks check, in one command.
# `--doc` is here because nextest cannot run doctests: it would otherwise be
# the one target nothing checks. Most rustdoc examples are ```ignore (they
# reference the consumer's own helpers), so this compiles the rest.
gate:
    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -W clippy::all -W clippy::pedantic -D warnings
    cargo nextest run --all-features
    cargo test --all-features --doc

# Dry-run publish check
publish-check:
    cargo publish --dry-run
