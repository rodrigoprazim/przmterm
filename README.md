# PrzmTerm

A native terminal client for network and systems engineers: SSH, Telnet and
serial consoles, a session tree with inherited settings, logging and automation.
Written in Rust, one binary per platform for macOS, Windows and Linux. No
Electron, no web view.

> **Status:** early development. 0.3.0 is out (SSH, tabs, logging, keychain);
> 0.4.0 adds Telnet and serial consoles. See the [roadmap](ROADMAP.md).

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
On macOS, unsigned development builds make the system ask for your login
keychain password (your Mac account password) after each rebuild; choose
*Always Allow*.

### Why did the login fail?

Turn on **Debug login** in the session editor (or `debug = true` in the
session file), or type `-v` before the target in Quick connect
(`-v admin@10.0.0.1`). Each step then shows in the terminal in cyan, like
`ssh -v`: the addresses tried, the server version and algorithms, the host key
and the known_hosts file it was found in, every key, password and
keyboard-interactive attempt with the server's answer and the methods it still
allows, and the reason the server gave if it disconnected.

### Tunnels and jump hosts

```toml
jump_host = "bastion"        # a saved session, or "user@host:port"

[[forward]]                  # ssh -L
listen = "5432"              # 127.0.0.1:5432 here…
target = "db.internal:5432"  # …reaches this, from the server's side

[[forward]]                  # ssh -R
type = "remote"
listen = "8022"              # port on the server…
target = "localhost:22"      # …reaches this, from here

[[forward]]                  # ssh -D
type = "dynamic"
listen = "1080"              # SOCKS 4/5 proxy here
```

The editor has the same under *Jump host* and *Tunnels*. A jump host's own
`jump_host` is followed too, so chains work. Tunnels open after login and
close with the session.

### Keepalive and reconnect

Sessions send a keepalive every 30 s (`keepalive`, 0 turns it off). With
**Auto reconnect** on (`auto_reconnect = true`), a pane whose connection drops,
or cannot be made, tries again after 2, 5, 10, 20 and then every 30 seconds,
on the same screen. Enter reconnects at once; Ctrl+C stops the retries. A
normal logout or a rejected login never triggers it.

### Telnet and serial

Quick connect also takes `telnet://host[:port]` and `serial://PORT[@BAUD]`,
for example `serial:///dev/ttyUSB0@115200` or `serial://COM3`. Telnet
negotiates echo, window size (updated on resize) and terminal type; the login
happens in the terminal. Serial sessions set baud rate, data bits, parity,
stop bits and flow control; the session editor lists the ports it finds
(on macOS the `/dev/cu.*` devices). On Linux your user needs to be in the
`dialout` group to open serial ports.

**Send BREAK** (Ctrl+Shift+B, ⌘⇧B, the *Break* button in the status bar, or
the right-click menu) holds the serial line in break for 300 ms, or sends
Telnet's BREAK command, e.g. for ROMMON password recovery.

### Host keys

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
| Send BREAK (serial, Telnet) | Ctrl+Shift+B | ⌘⇧B |

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
debug = true               # show each step of the SSH login (like ssh -v)
keepalive = 30             # seconds between keepalives, 0 = off
auto_reconnect = true      # reconnect when the connection drops
serial_port = "/dev/ttyUSB0"   # COM3 on Windows, /dev/cu.usbserial-… on macOS
baud_rate = 9600
data_bits = 8              # 5-8
parity = "none"            # none | odd | even
stop_bits = 1              # 1 | 2
flow_control = "none"      # none | software (XON/XOFF) | hardware (RTS/CTS)

[terminal]
term_type = "xterm-256color"
scrollback = 10000

[log]
enabled = true
dir = "~/logs"                 # folder for relative file names (default <config>/logs)
path = "%S-%Y%M%D.log"         # %S session, %H host, %Y%M%D date, %h%m%s time
```

Sessions can also be created and edited from the app: **+** next to
*Sessions*, or right-click a session for Edit, Duplicate and Delete. Right-click
a folder to add a session or subfolder there, rename it, or edit the settings
its sessions inherit.

Global defaults live in `defaults.toml` (⚙ next to *Sessions* in the app).
Changing them asks whether existing sessions that use the defaults should
follow; if not, PrzmTerm writes the old value into those sessions.

Unlock the padlock next to *Sessions* to drag sessions between folders.

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
