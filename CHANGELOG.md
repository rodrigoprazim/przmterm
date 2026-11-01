# Changelog

All notable changes to PrzmTerm are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/) as described in
[docs/VERSIONING.md](docs/VERSIONING.md).

## [Unreleased]

### Added

- SSH sessions: password, public key files, ssh-agent (Pageant and the OpenSSH
  agent on Windows) and keyboard-interactive (2FA) authentication.
- Host keys are checked against PrzmTerm's own `known_hosts` and, read-only,
  `~/.ssh/known_hosts`. Unknown keys are confirmed in the terminal; a changed
  key blocks the connection and shows both fingerprints.
- Host key, `login as:`, password and passphrase prompts appear in the terminal.
- Quick-connect field (`user@host:port`, `telnet://host`) and double-click to
  connect from the session tree. Press Enter in a closed session to reconnect.
- `identity_file` session setting.

### Changed

- The 16 ANSI colours use a palette that stays readable on the dark background.

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
