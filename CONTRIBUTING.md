# Contributing to PrzmTerm

## Before a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same checks on Linux, macOS and Windows, plus a build with the
minimum Rust version from `Cargo.toml`.

## Conventions

- **Layers.** A crate depends only on crates below it:
  `core` ← `transport`, `term`, `session` ← `cli`, `app`. Front ends never talk
  to a transport except through a session.
- **Commands first.** A new user action is a command on the bus, then a menu
  item or shortcut that calls it.
- **Tests.** Emulator changes come with a test that feeds bytes and checks the
  screen. Bug fixes come with a regression test.
- **Changelog.** User-visible changes add a line under `[Unreleased]` in
  `CHANGELOG.md`.
- **Commits.** Short imperative subject with a type prefix:
  `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `ci:`, `chore:`, `release:`.
- **Versions.** See [docs/VERSIONING.md](docs/VERSIONING.md).

## Clean-room rule

PrzmTerm reimplements workflows found in commercial terminal clients. Work only
from public documentation, published standards (ECMA-48, VT manuals, RFCs) and
your own use of the products. Do not copy, decompile or paraphrase another
vendor's code, icons, help text or other assets, and do not use their product
names in PrzmTerm's branding. Reading another product's documented file
formats to import user data is fine.
