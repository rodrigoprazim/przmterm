# PrzmTerm

A native terminal client for network and systems engineers: SSH, Telnet and
serial consoles, a session tree with inherited settings, logging and automation.
Written in Rust, one binary per platform for macOS, Windows and Linux. No
Electron, no web view.

> **Status:** early development. 0.1.0 is out; 0.2.0 adds SSH. Telnet and
> serial arrive in 0.4.0. See the [roadmap](ROADMAP.md).

## Goals

- **Native and fast.** egui on the GPU; a dedicated grid renderer before 1.0.
- **Built for network gear.** Serial with BREAK, Telnet, legacy SSH algorithms
  per session, SecureCRT session import.
- **Everything is a command.** The GUI, the `przm` CLI, scripts and an MCP server
  call the same command bus.
- **Plain-text configuration.** Sessions are TOML files you can keep in Git.
  Passwords live in the OS keychain, never in the files.

## Build

Requires Rust 1.95 or newer.

```sh
git clone https://github.com/rodrigoprazim/przmterm
cd przmterm
cargo run --release            # desktop app (przmterm)
cargo run --release -p przmterm-cli -- --help   # CLI (przm)
```

## Connecting

Type `user@host:port` in **Quick connect**, or double-click a saved session.
Host key questions, `login as:`, passwords, key passphrases and two-factor
prompts appear in the terminal, as with OpenSSH. Authentication tries, in
order: ssh-agent (Pageant or the OpenSSH agent on Windows), the session's
`identity_file`, `~/.ssh/id_ed25519`, `id_ecdsa`, `id_rsa`, keyboard-interactive
and password.

Passwords are never written to session files. After a typed password works,
PrzmTerm offers to save it in the OS keychain (Keychain on macOS, Credential
Manager on Windows, Secret Service on Linux); right-click a session to forget it.

Accepted host keys go to `known_hosts` in the configuration directory (OpenSSH
format). `~/.ssh/known_hosts` is also consulted, read-only. If a host's key
changes, PrzmTerm refuses to connect and shows the old and new fingerprints.

## Keyboard and mouse

| Action | Linux / Windows | macOS |
| --- | --- | --- |
| New tab (same session, no new login) | Ctrl+Shift+T | ⌘⇧T |
| Split side by side / stacked | Ctrl+Shift+D / Ctrl+Shift+E | ⌘⇧D / ⌘⇧E |
| Close pane | Ctrl+Shift+W | ⌘⇧W |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab | same |
| Copy | Ctrl+Shift+C, or Ctrl+C with a selection | ⌘C |
| Paste | Ctrl+Shift+V or Ctrl+V | ⌘V |
| Scroll back | wheel, Shift+PageUp/PageDown, Shift+Home/End | same |

Drag to select, double-click selects a word, triple-click a line. Right-click
opens Copy / Paste / Select all.

## Sessions

PrzmTerm reads its configuration from `$PRZMTERM_HOME`, or by default from:

| OS | Directory |
| --- | --- |
| Linux | `~/.config/przmterm` |
| macOS | `~/Library/Application Support/przmterm` |
| Windows | `%APPDATA%\przmterm` |

```text
przmterm/
  defaults.toml           # global defaults
  sessions/
    core-sw-01.toml
    Cisco/
      folder.toml         # inherited by everything in Cisco/
      edge-rtr-01.toml
```

Each file can set any of these keys; anything unset is inherited from the
folder above, then from `defaults.toml`:

```toml
protocol = "ssh"          # ssh | telnet | serial | local
host = "10.0.0.1"
port = 22
username = "admin"
identity_file = "~/.ssh/id_lab"
save_password = true       # keychain: unset asks, true always, false never
serial_port = "/dev/ttyUSB0"
baud_rate = 9600

[terminal]
term_type = "xterm-256color"
scrollback = 10000

[log]
enabled = true
path = "~/logs/%S-%Y%M%D.log"   # %S session, %H host, %Y%M%D date, %h%m%s time
```

Sessions can also be created and edited from the app: **+** next to
*Sessions*, or right-click a session for Edit, Duplicate and Delete. Right-click
a folder to add a session or subfolder there, rename it, or edit the settings
its sessions inherit.

A sample tree is in [examples/config](examples/config). Try it with
`PRZMTERM_HOME=examples/config cargo run`.

## Workspace

| Crate | Role |
| --- | --- |
| `przmterm-core` | Command bus, shared types |
| `przmterm-transport` | `Transport` trait; SSH, Telnet, serial and local implementations |
| `przmterm-term` | Escape-sequence parser, grid, scrollback |
| `przmterm-session` | Session files and inheritance |
| `przmterm-cli` | The `przm` binary |
| `przmterm-app` | The `przmterm` desktop binary |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). PrzmTerm is a clean-room project: it
reproduces workflows from public documentation and everyday use, never from
another vendor's code or assets.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT) at your option.
