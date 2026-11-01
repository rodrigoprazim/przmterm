# PrzmTerm

A native terminal client for network and systems engineers: SSH, Telnet and
serial consoles, a session tree with inherited settings, logging and automation.
Written in Rust, one binary per platform for macOS, Windows and Linux. No
Electron, no web view.

> **Status:** early development, working toward **0.1.0**. SSH arrives in 0.2.0.
> See the [roadmap](ROADMAP.md).

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
serial_port = "/dev/ttyUSB0"
baud_rate = 9600

[terminal]
term_type = "xterm-256color"
scrollback = 10000

[log]
enabled = true
path = "~/logs/%S.log"
```

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
