# Contributing to PLOMID

PLOMID is developed as a Rust workspace. Read the [coding standards](docs/coding-standards.md)
and the [ADR process](docs/adr/README.md) before making changes.

## Local development

From the repository root, run: 

```bash
cargo build --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```

Install `cargo-deny` if it is not already available. CI runs the same quality
gates, including the dependency and license policy.

## Pull requests

- Keep each pull request focused on one task and preserve the dependency order
  of the roadmap.
- Add or update tests for behavior changes. Storage, WAL, recovery, MVCC, and
  transaction changes require explicit correctness and failure-path coverage.
- Document public API or architectural changes. Add an ADR for a material
  design decision and link it in the pull request.
- Explain validation performed, known limitations, and any follow-up work.
- Do not add secrets or forbidden external database foundations.

Pull requests require review from the owners in [`CODEOWNERS`](CODEOWNERS) and
must pass the required CI checks.
