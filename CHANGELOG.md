# Changelog

All notable changes to PrzmTerm are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/) as described in
[docs/VERSIONING.md](docs/VERSIONING.md).

## [Unreleased]

### Added

- Tabs and split panes (side by side or stacked). A new tab or pane opens the
  same session. Shortcuts: Ctrl+Shift+T / ⌘⇧T new tab, Ctrl+Shift+D and
  Ctrl+Shift+E split, Ctrl+Shift+W close pane, Ctrl+Tab next tab.
- Mouse selection: drag, double-click for a word (IP addresses and paths stay
  whole), triple-click for a line. Copy with Ctrl+Shift+C, or Ctrl+C when text
  is selected; paste with Ctrl+Shift+V or Ctrl+V (⌘C / ⌘V on macOS); right-click menu.
- Scrollback browsing with the mouse wheel and Shift+PageUp/PageDown/Home/End.
  On full-screen programs (less, vim) the wheel sends arrow keys.
- Wide characters (CJK, emoji) take two cells.
- DEC Special Graphics line-drawing charset.
- Per-session plain-text logs: escape sequences removed, `\r` progress lines
  collapsed. File names take `%S` (session), `%H` (host), `%Y %M %D`,
  `%h %m %s`; relative paths go under `<config>/logs`. The status bar toggles
  logging for the active pane.
- Saved passwords in the OS keychain (Keychain on macOS, Credential Manager on
  Windows, Secret Service on Linux). After a typed password works, PrzmTerm
  asks whether to save it; a saved password that stops working is forgotten.
  The `save_password` setting makes this always or never.
- Session editor: create, edit, duplicate and delete sessions from the tree
  (right-click, or + for a new one). Only values set in the file are written;
  empty fields inherit.

## [0.2.0] - 2026-11-01

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

[Unreleased]: https://github.com/rodrigoprazim/przmterm/compare/v0.2.0...main
[0.1.0]: https://github.com/rodrigoprazim/przmterm/releases/tag/v0.1.0
[0.2.0]: https://github.com/rodrigoprazim/przmterm/compare/v0.1.0...v0.2.0
