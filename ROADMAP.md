# PrzmTerm roadmap

PrzmTerm ships as a series of versions from **0.1.0** to **1.0.0**. Each
version has a theme and a gate: it is released when the gate is met, not on a
date. Version numbering and the release procedure are in
[docs/VERSIONING.md](docs/VERSIONING.md).

| Version | Theme | Status |
| --- | --- | --- |
| 0.1.0 | Foundation | Released |
| 0.2.0 | SSH | Released |
| 0.3.0 | Daily-driver MVP | Released |
| 0.4.0 | Telnet and serial | Released |
| 0.5.0 | Network gear | In progress |
| 0.6.0 | Productivity | Planned |
| 0.7.0 | Scripting | Planned |
| 0.8.0 | File transfer | Planned |
| 0.9.0 | Automation and hardening | Planned |
| 1.0.0 | Stable | Planned |

## 0.1.0 — Foundation

- [x] Cargo workspace with layered crates and shared lints
- [x] Command bus (`przmterm-core`)
- [x] `Transport` trait with an in-memory loopback (`przmterm-transport`)
- [x] Terminal emulator on `vte`: cursor movement, erase, insert/delete, scroll
      regions, SGR 16/256/truecolor, alternate screen, DSR/DA replies
      (`przmterm-term`)
- [x] Session files in TOML with three-level inheritance (`przmterm-session`)
- [x] `przm` CLI: `sessions`, `render`
- [x] Desktop app: session tree, terminal widget, demo shell behind `Transport`
- [x] CI on Linux, macOS and Windows; release workflow on version tags
- [x] CI green on all three platforms

**Gate:** CI green on all three platforms and the demo shell runs on each.

## 0.2.0 — SSH

- [x] SSH transport on `russh`: password, public key, ssh-agent / Pageant, keyboard-interactive
- [x] `known_hosts` compatible with OpenSSH; block and show both fingerprints on a changed key
- [x] One Tokio task per session; the GUI wakes on incoming data
- [x] Connect from the session tree (double-click) and from a quick-connect field
- [x] Prompts (host key, `login as:`, password, passphrase, 2FA) answered in the terminal; nothing stored yet
- [x] Integration tests against an in-process SSH server
- [x] Gate checked on macOS (Windows: CI build and tests only)

**Gate:** `vim`, `htop` and `less` run over SSH without rendering artefacts.
Checked on Linux against OpenSSH 9.6 and on macOS against the maintainer's servers.

## 0.3.0 — Daily-driver MVP

- [x] Tabs and split panes
- [x] Mouse selection, copy and paste; scrollback browsing with the wheel
- [x] Wide (CJK) characters and the DEC line-drawing charset
- [x] Per-session logging with file-name templates
- [x] Credentials in the OS keychain (`keyring`)
- [x] Session editor in the GUI (create, edit, duplicate, delete)
- [x] Gate checked on macOS (Windows and Linux: CI build and tests only)

**Gate:** used every day instead of SecureCRT for SSH sessions.
Checked by the maintainer on macOS against his own servers, including 2FA logins.

## 0.4.0 — Telnet and serial

- [x] Telnet with option negotiation (NAWS, TTYPE, ECHO, SGA)
- [x] Serial console: baud rate, parity, stop bits, flow control, BREAK; port enumeration
- [x] Gate checked: Telnet on the maintainer's equipment; serial against a
      virtual cable and in tests (real USB adapter still to be confirmed)

**Gate:** a Telnet login and a serial console (USB adapter) to network
equipment work for a full session, including BREAK. Telnet was checked on real
equipment; the serial console has only been checked against a virtual cable.

## 0.5.0 — Network gear

- Import sessions from SecureCRT
- Jump hosts (ProxyJump, chained) and tunnels: port forwarding (local, remote,
  dynamic / SOCKS)
- X11 forwarding: remote graphical programs shown on the local X server
  (XQuartz on macOS, VcXsrv or similar on Windows)
- Legacy SSH algorithms enabled per session, with a visible "legacy" badge;
  libssh2 fallback where `russh` lacks an algorithm
- Keepalive and automatic reconnect

**Gate:** connects to every device in the reference lab.

## 0.6.0 — Productivity

- Button bar with macro buttons organised in tabs (one tab per device type,
  project, etc.): a button sends text or a command sequence to the current
  session, to several sessions at once, or runs a script
- Send input to several tabs at once
- Keyword highlighting and regex triggers
- Password vault: an encrypted file (master password, Argon2id +
  XChaCha20-Poly1305) holding user names, passwords and key passphrases.
  Sessions and folders refer to a vault entry instead of storing a user name,
  so one credential change updates every device that uses it. Works where no
  OS keychain is available and can be copied between machines; the OS
  keychain stays available as an alternative.
- Settings backup and export. Backup: one archive with global settings,
  folders, sessions and known hosts, restorable on this or another machine.
  Export: a chosen set of folders or sessions to share with someone else,
  without passwords or vault entries, imported into their tree as a new folder.

## 0.7.0 — Scripting

- Embedded scripting with a `session` object (`send`, `wait_for`, `read_until`, `log`, `prompt`).
  Python via `pyo3` as an optional feature is the current plan; Lua/Rhai is the
  lighter alternative (decision open)

## 0.8.0 — File transfer

- SFTP with a file browser
- Xmodem, Ymodem and Zmodem

## 0.9.0 — Automation and hardening

- `przm run <session> --cmd …` headless mode
- MCP server: `list_sessions`, `open_session`, `send`, `read_screen`, `run_script`,
  with per-session permissions and a command block list
- Dedicated `wgpu` grid renderer with a glyph atlas
- `cargo audit` / `cargo deny` in CI; signed and notarised binaries
- Release candidates `1.0.0-rc.N`

## 1.0.0 — Stable

- Configuration format declared stable (compatibility promise for 1.x)
- Signed binaries for macOS (universal), Windows and Linux
- User guide and scripting reference

**Gate:** real scripts migrated from SecureCRT and no open critical bugs.

## After 1.0

Kerberos/GSSAPI, smart cards (PKCS#11), session sync through
Git, inventory import (Ansible, Zabbix, NetBox).
