# Everything CI checks (.github/workflows/ci.yml, job `gate`), in one command.
# `--doc` is here because nextest cannot run doctests: it would otherwise be
# the one target nothing checks.
gate:
    cargo fmt --all -- --check
    mbx clippy --all-targets --all-features -- -W clippy::all -W clippy::pedantic -D warnings
    mbx clippy --all-targets -- -W clippy::all -W clippy::pedantic -D warnings
    mbx nextest run --all-features
    mbx test --all-features --doc
    ast-grep scan --error
    ! grep -rnE '#!?\[(allow|expect)\(|ast-grep-ignore|rustfmt::skip' src tests examples benches
    RUSTDOCFLAGS="-D warnings" mbx doc --no-deps --all-features

# Dry-run publish check
publish-check:
    mbx publish --dry-run
