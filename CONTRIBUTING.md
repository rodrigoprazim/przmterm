# Contributing to PrzmTerm

## Before a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same checks on Linux, macOS and Windows, plus a build with the
minimum Rust version from `Cargo.toml` and `cargo deny check` (RustSec
advisories, licences, crate sources; settings in `deny.toml`). A new
dependency must have a licence listed there, and an advisory that does not
apply needs an entry with the reason.

## Conventions

- **Layers.** A crate depends only on crates below it:
  `core` ← `transport`, `term`, `session` ← `connect` ← `cli`, `app`. Front
  ends never talk to a transport except through a session; `connect` turns a
  session into its transport (jump hosts, vault entries, saved passwords) for
  both.
- **Commands first.** A new user action is a command on the bus, then a menu
  item or shortcut that calls it.
- **Tests.** Emulator changes come with a test that feeds bytes and checks the
  screen. Bug fixes come with a regression test.
- **Changelog.** User-visible changes add a line under `[Unreleased]` in
  `CHANGELOG.md`.
- **Commits.** Short imperative subject with a type prefix:
  `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `ci:`, `chore:`, `release:`.
- **Versions.** See [docs/VERSIONING.md](docs/VERSIONING.md).
- **Configuration files.** New keys only; never rename, remove or change the
  meaning of one in 1.x. Update [docs/CONFIG.md](docs/CONFIG.md) with the
  change.

## Clean-room rule

PrzmTerm reimplements workflows found in commercial terminal clients. Work only
from public documentation, published standards (ECMA-48, VT manuals, RFCs) and
your own use of the products. Do not copy, decompile or paraphrase another
vendor's code, icons, help text or other assets, and do not use their product
names in PrzmTerm's branding. Reading another product's documented file
formats to import user data is fine.

## Icons

The interface's icons are [Lucide](https://lucide.dev) (ISC licence), from
a font cut down to the icons in use: `assets/fonts/lucide-icons.ttf`. To
add one, put its Lucide name in `scripts/lucide-icons.py`, run it on the
`lucide-static` npm package (the script says how) and add the icon with its
code point to `Icon` in `crates/przmterm-app/src/icons.rs`; a test checks
that the font has every icon.

## Installer artwork

The disk image background (`packaging/macos/`) and the setup pictures
(`packaging/windows/*.bmp`) are drawn by `scripts/installer-art.py` from the
app icon and Inter; change the script and run it again rather than editing
the pictures. The icon positions in `packaging/macos/dmg-settings.py` must
match the ones the script draws the arrow between.
