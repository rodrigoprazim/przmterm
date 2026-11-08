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
| 0.5.0 | Network gear | Released |
| 0.6.0 | Productivity | Released |
| 0.7.0 | Scripting | Released |
| 0.8.0 | File transfer | Released |
| 0.9.0 | Automation and hardening | Released |
| 1.0.0 | Stable | Next |

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

- [x] Import sessions from SecureCRT
- [x] Jump hosts (ProxyJump, chained) and tunnels: port forwarding (local, remote,
  dynamic / SOCKS)
- [x] X11 forwarding: remote graphical programs shown on the local X server
  (XQuartz on macOS, VcXsrv or similar on Windows)
- [x] Legacy SSH algorithms enabled per session, with a visible "legacy" badge
      (`russh` covers SHA-1 key exchange, CBC and 3DES, HMAC-SHA1, `ssh-rsa`
      and DSA; a libssh2 fallback moved to after 1.0, only if equipment needs
      an algorithm `russh` lacks)
- [x] Keepalive and automatic reconnect
- [x] Gate checked: SSH, jump hosts, tunnels and X11 on the maintainer's
      machines; legacy algorithms against an OpenSSH limited to them (real
      old equipment still to be confirmed)

**Gate:** connects to every device in the reference lab. Checked on the
maintainer's servers; legacy algorithms only against a test server.

## 0.6.0 — Productivity

- [x] Button bar with macro buttons organised in tabs (one tab per device type,
  project, etc.): a button sends text or a command sequence to the current
  session, with a description and optional tooltips (running a script waits
  for 0.7.0)
- [x] Send input to every pane of a split tab at once, after a warning
- [x] Tab menu (right-click a tab): rename the tab (until it closes), reset the
  name, reconnect, disconnect, close (connected sessions ask first), close
  disconnected / other / right-hand / all tabs, clone, session options (or
  save a quick-connect session), font size, send commands to the tab
- [x] Keyword highlighting: named keyword sets (words or regular expressions, each
  with its colours), chosen per session or folder; SecureCRT keyword lists
  (`Keywords/*.ini`, e.g. a `CORES` set) can be imported
- [x] Keyword set editor in the app: create, rename and delete sets; add, reorder
  and remove rules with colour pickers and a live preview
- [x] Triggers: text or regular expressions in the output answered
  automatically (`--More--` → space, `[confirm]` → Enter), once or every time
- [x] Password vault: an encrypted file (master password, Argon2id +
  XChaCha20-Poly1305) holding user names, passwords and key passphrases.
  Sessions and folders refer to a vault entry instead of storing a user name,
  so one credential change updates every device that uses it. Works where no
  OS keychain is available and can be copied between machines; the OS
  keychain stays available as an alternative.
- [x] Settings backup and export. Backup: one archive with global settings,
  folders, sessions and known hosts, restorable on this or another machine.
  Export: a chosen set of folders or sessions to share with someone else,
  without passwords or vault entries, imported into their tree as a new folder.
- [x] Split joins sessions open in other tabs (grid of two per row, side by
  side, stacked); panes can be rearranged or separated into tabs again
- [x] Gate checked on macOS by the maintainer: buttons, keyword sets,
      triggers, tab menu, split and the vault in daily use

**Gate:** the maintainer's daily SecureCRT conveniences (buttons, keyword
highlighting, triggers, saved credentials) work in PrzmTerm.

## 0.7.0 — Scripting

- [x] Keymap editor: assign keyboard shortcuts to sending text, app commands
  (new tab, split, send BREAK…), buttons and scripts

- [x] Embedded scripting in Rhai (chosen over Python and Lua: pure Rust,
  nothing to install) with a `session` object (`send`, `wait_for` with one or
  several texts and a timeout, `read_until`, `name`, `host`, `username`) and
  `log`/`print`, `prompt`, `prompt_secret`, `sleep`; run from the tab menu,
  from a button or as a session's logon script, and stopped at any time
- [x] Gate checked on macOS by the maintainer: scripts (Linux samples) and
      keyboard shortcuts

**Gate:** the maintainer's own scripts and shortcuts work on his devices.

## 0.8.0 — File transfer

- [x] SFTP with a file browser, also opened from a tab's menu ("File
  Browser › SFTP"), from the session list and with a shortcut, on the same
  connection (no new login); uploads (also by dropping files) and downloads
  of files and whole folders with progress and cancel, new folder, rename,
  delete
- [x] Xmodem, Xmodem-1K, Ymodem and Zmodem, sending and receiving, checked
  against lrzsz; a Zmodem start from `sz` or `rz` on the other side is
  picked up on its own
- [x] SCP on the same SSH connection: a file browser like the SFTP one
  ("File Browser › SCP") and transfers from the File transfer dialog, with
  the commands run in a shell chosen per session (`/bin/bash`, `sudo su -`…)

**Gate:** files moved to and from the maintainer's servers (SFTP, Zmodem)
and, when a device is at hand, a serial console (Xmodem or Ymodem).

- [x] Gate checked on macOS by the maintainer: SFTP, Zmodem (with lrzsz)
      and SCP on his servers; no serial device at hand for this release

## 0.9.0 — Automation and hardening

- [x] Tabs and windows: tab groups (close a group, move tabs to a new group, send
  commands to a group), several windows (clone or move a tab to a new
  window), locking a session with a password
- [x] Font family and size saved per session, with the cursor shape, colour
  and blinking
- [x] Anti-idle: a text sent after N seconds without typing
- [x] Settings folder can be moved (a synced folder shares it between computers)
- [x] The opening tab is a PrzmTerm shell: `ssh <session>` with Tab
  completion, `ssh user@host`, `telnet`, `sessions`, history
- [x] `przm run <session> --cmd …` headless mode
- [x] MCP server: `list_sessions`, `open_session`, `send`, `read_screen`, `run_script`,
  with per-session permissions and a command block list
- [x] Dedicated `wgpu` grid renderer with a glyph atlas
- [x] `cargo deny` in CI (RustSec advisories as `cargo audit` checks them,
  licences, crate sources), also weekly
- [x] Release signing with free services: Windows through the SignPath
  Foundation (version information in the executables, the artifact
  configuration, the approval step), macOS ad-hoc signed (notarising needs a
  paid Apple account), checksums signed with GPG; install notes and a code
  signing policy in the README
- [x] macOS release as `PrzmTerm.app` on a disk image, with an icon (also on
  the window and the Windows executables)
- [x] Release key set up (`docs/release-key.asc`)

## 1.0.0 — Stable

- SignPath Foundation approval for the Windows signing (applied for on
  2026-11-08)
- Release candidates `1.0.0-rc.N`
- Configuration format declared stable (compatibility promise for 1.x)
- Signed binaries: Windows through the SignPath Foundation, macOS
  (universal) ad-hoc signed, checksums signed with GPG
- User guide and scripting reference

**Gate:** the maintainer's own automation scripts in daily use and no open
critical bugs.

## After 1.0

More button functions, as in SecureCRT's *Map Button*: send to the command
window, credentials, menu and scroll bar functions, Telnet/SSH/VT/TN3270
functions, switching button bars, launching an application,
and button icon colours.

libssh2 fallback for SSH algorithms `russh` lacks (only if real equipment
needs one), Kerberos/GSSAPI, smart cards (PKCS#11), session sync through
Git, inventory import (Ansible, Zabbix, NetBox).
