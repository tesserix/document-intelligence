# Contributing

Thanks for helping improve the Tesserix document-intelligence service.

## Before you start

- Discuss larger changes in an issue first so the design can be agreed before code is written.
- Security issues go through [SECURITY.md](SECURITY.md), never a public issue.
- Read [`docs/security/THREAT-MODEL.md`](docs/security/THREAT-MODEL.md); most review feedback on this repository is about trust boundaries.

## Development

The workspace targets the Rust toolchain pinned in `.github/workflows/ci.yml`.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```

Database-backed tests need a local Postgres; `scripts/setup-test-database.sh` prepares it.

## Pull requests

- Branch from `main`; one logical change per pull request.
- Follow test-first development: bug fixes include a regression test, features include behaviour tests.
- Use [Conventional Commits](https://www.conventionalcommits.org/) subjects under 72 characters.
- Keep comments to one line and explain *why*, not *what*.
- Never commit secrets, credentials, or document content. Push protection and secret scanning are enabled and will reject them.
- CI must be green. Pull requests from first-time contributors need a maintainer to approve workflow runs.
- A CODEOWNERS review is required to merge into `main`.

## Dependencies

New dependencies must be compatible with Apache-2.0 and pass `cargo deny check`
(see `deny.toml`). Prefer crates that are already in the workspace.

## Developer Certificate of Origin

By contributing you certify the [Developer Certificate of Origin 1.1](https://developercertificate.org/):
that you wrote the change or have the right to submit it under the
Apache-2.0 license. Sign your commits with `git commit -s`.

## License

Contributions are licensed under the [Apache License 2.0](LICENSE).
