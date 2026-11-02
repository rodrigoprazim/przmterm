# Changelog

All notable changes to PrzmTerm are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/) as described in
[docs/VERSIONING.md](docs/VERSIONING.md).

## [Unreleased]

### Added

- Keepalive interval per session (`keepalive`, seconds; 0 turns it off):
  SSH keepalives, Telnet NOPs. Default 30 s.
- Import from SecureCRT (**+** › *Import from SecureCRT…*, or
  `przm import-securecrt`): the XML export, session `.ini` files or the whole
  `Config/Sessions` folder go into a `SecureCRT` folder with the same
  sub-folders. Taken over: protocol (SSH, Telnet, serial), host, port, user
  name, key file, jump host, serial line, terminal type, scrollback, keepalive
  and logging. Passwords are not imported (they stay encrypted in SecureCRT);
  sessions with logon scripts, port forwards or unsupported protocols (RDP,
  local shell) are listed in the summary.
- Tunnels (`[[forward]]` in the session file, *Tunnels* in the editor):
  local (`-L`), remote (`-R`) and dynamic SOCKS 4/4a/5 (`-D`) port
  forwarding, opened after login and closed with the session. A port already
  in use is reported without ending the session.
- Jump hosts (`jump_host`): connect through another SSH server first, named
  by saved session or as `user@host:port`; a jump host's own jump host is
  used too (chained). Each hop asks for its own login in the terminal.
- Automatic reconnect (`auto_reconnect`): when a connection drops or cannot
  be made, the pane reconnects after 2, 5, 10, 20, then every 30 s. Enter
  reconnects at once, Ctrl+C stops it. Not after a normal logout or a failed
  login. Unplugging and replugging a USB serial adapter reconnects too.

### Changed

- Reconnecting (Enter, or automatically) keeps the screen, scrollback and log
  instead of starting a blank pane.

## [0.4.0] - 2026-11-01

### Added

- Telnet sessions with option negotiation: the server echoes (ECHO),
  character mode (SGA), 8-bit (BINARY), terminal type (TTYPE) and window size
  (NAWS, sent again on resize). Unknown options are refused without loops.
- Serial console sessions: baud rate, data bits, parity, stop bits and flow
  control (XON/XOFF or RTS/CTS). The session editor lists the serial ports
  found on the machine (USB adapter name included) and common baud rates.
- Send BREAK on serial (300 ms line break) and Telnet sessions:
  Ctrl+Shift+B / ⌘⇧B, the *Break* button in the status bar, or the right-click menu.
- Quick connect accepts `serial://PORT[@BAUD]`, e.g. `serial:///dev/ttyUSB0@115200`.
- New session settings `data_bits`, `parity`, `stop_bits` and `flow_control`.
- SSH login debugging, like `ssh -v`: the **Debug login** setting (`debug`)
  or `-v` in Quick connect shows each step in the terminal — addresses,
  server version, negotiated algorithms, host key and known_hosts file, every
  key, password and keyboard-interactive attempt with the server's answer and
  remaining methods, and the server's disconnect reason.

### Changed

- The session editor shows only the fields of the chosen protocol. Folder and
  global settings keep the other protocols' fields under a collapsed section.
- A split or new tab of a serial session explains that the port is already
  open instead of failing to open it twice.
- Messages such as "Saved …" or "BREAK sent." appear in the status bar for a
  few seconds instead of under *Sessions*.
- Long session names stay on one line; the session list scrolls sideways
  instead of wrapping them.
- The session list can be hidden for a wider terminal: ◀ next to *Sessions*,
  ▶ to bring it back, or Ctrl+Shift+S / ⌘⇧S.

### Fixed

- Renaming a folder or session by changing only letter case (`GeOFibra` →
  `GeoFibra`) failed with "already exists" on macOS and Windows; renaming a
  session that way could lose its file. Saving a session never overwrites
  another one.
- Characters a file name cannot have (`/ \ : * ? " < > |`) are dropped while
  a session or folder name is typed, with a note, instead of failing on save.
- A key file set to the public key (`.pub`) uses the private key next to it;
  without one, the error says to choose the private key. The file dialog
  does the same.

## [0.3.0] - 2026-11-01

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
  `%h %m %s`; relative paths go under `<config>/logs`. The status bar button
  toggles logging for the active pane: green while logging, red while not.
- Saved passwords in the OS keychain (Keychain on macOS, Credential Manager on
  Windows, Secret Service on Linux). After a typed password works, PrzmTerm
  asks whether to save it; a saved password that stops working is forgotten.
  The `save_password` setting makes this always or never.
- Session editor: create, edit, duplicate and delete sessions from the tree
  (right-click, or + for a new one). Only values set in the file are written;
  empty fields show the value they inherit in grey. The folder
  is chosen from a compact tree, the terminal type from a list, and the key
  file with the system file dialog.
- Folders: right-click a folder to add a session or a subfolder, rename it,
  delete it, or edit its settings (`folder.toml`), which the sessions inside inherit.
- New tabs and split panes open another channel on the existing SSH connection,
  so there is no second login or two-factor prompt.
- Two panes logging the same session write to separate files (`-2`, `-3`…).
  If the server refuses a second session on one connection, the new pane
  logs in on its own and says why. Opening a session that is already
  connected also reuses its connection.
- Global settings (⚙ next to *Sessions*, saved in `defaults.toml`). Saving a
  change asks for confirmation and whether existing sessions that use the
  default should follow it; if not, they keep their current value.
- Log folder setting (`[log] dir`) with a folder picker, and a `?` next to the
  log file name listing the variables, examples and the next file name.
- Drag sessions onto folders (or the top level) to move them, after unlocking
  the padlock next to *Sessions*.
- Enter saves and Esc cancels in the editor and the folder dialogs. Dialogs
  open in the middle of the window.

### Fixed

- A password followed by a two-factor prompt (keyboard-interactive in two
  rounds) was never saved, and a saved password was wrongly dropped when the
  token prompt came. The password is now saved and reused; the token is always asked.
- macOS: saving a password failed with "already exists" when an earlier build
  had saved it; the old item is now replaced.

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

[Unreleased]: https://github.com/rodrigoprazim/przmterm/compare/v0.4.0...main
[0.1.0]: https://github.com/rodrigoprazim/przmterm/releases/tag/v0.1.0
[0.2.0]: https://github.com/rodrigoprazim/przmterm/compare/v0.1.0...v0.2.0
[0.3.0]: https://github.com/rodrigoprazim/przmterm/compare/v0.2.0...v0.3.0
[0.4.0]: https://github.com/rodrigoprazim/przmterm/compare/v0.3.0...v0.4.0
