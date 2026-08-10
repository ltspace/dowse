# Contributing

## Prerequisites

- Rust 1.85+ (edition 2024), install via [rustup](https://rustup.rs)
- Node 22+ (only needed for `dowse-app`'s frontend)
- Windows (dowse is Windows-native; Windows.Media.Ocr and the NTFS-specific paths do not build elsewhere)

## Building

```powershell
cargo build --workspace
```

## Before you open a PR

Rust checks:

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Frontend checks:

```powershell
cd crates/dowse-app
npm ci
npm run test:unit
npm run check
npm run build
```

CI runs the Rust commands on `windows-latest` and the frontend commands in a
separate Linux job. The automated frontend build cannot verify Windows Acrylic,
window behavior, or animation quality, so UI changes still need a manual Windows
smoke test before merge.

## Commit style

[Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`, ...). Commit messages in Chinese are fine — clarity matters more than language. Keep the subject line short; put the "why" in the body if it's not obvious.

## Pull requests

1. Fork and branch off `main`.
2. Keep the diff focused — one logical change per PR.
3. Make sure the Rust and frontend checks above pass locally before pushing.
4. Describe what changed and why in the PR description; link any related issue.
5. One maintainer review is required before merge.

## License

By contributing, you agree your contributions are dual-licensed under [MIT](LICENSE-MIT) and [Apache-2.0](LICENSE-APACHE), same as the rest of the project.
