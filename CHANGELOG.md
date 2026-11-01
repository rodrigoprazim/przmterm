# Changelog

All notable changes to PrzmTerm are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/) as described in
[docs/VERSIONING.md](docs/VERSIONING.md).

## [Unreleased]

## [0.1.0] - 2026-11-01

### Added

- Cargo workspace with `core`, `transport`, `term`, `session`, `cli` and `app` crates.
- Command bus shared by every front end.
- `Transport` trait and an in-memory loopback transport.
- Terminal emulator: cursor movement, erase, insert/delete, scroll regions,
  SGR with 16/256/truecolor, alternate screen, DECCKM, bracketed paste,
  cursor position and device attribute replies.
- Session files in TOML with global, folder and session inheritance.
- `przm` CLI with `sessions` and `render` commands.
- Desktop app with a session tree, terminal widget, xterm key mapping and a demo shell.
- CI on Linux, macOS and Windows and a release workflow for version tags.

[Unreleased]: https://github.com/rodrigoprazim/przmterm/compare/v0.1.0...main
[0.1.0]: https://github.com/rodrigoprazim/przmterm/releases/tag/v0.1.0
