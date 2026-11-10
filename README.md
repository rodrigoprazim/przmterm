# PrzmTerm

A native terminal client for network and systems engineers: SSH, Telnet and
serial consoles, a session tree with inherited settings, logging and automation.
Written in Rust, one binary per platform for macOS, Windows and Linux. No
Electron, no web view.

> **Status:** 0.9.0 is out; 1.0.0 is being prepared through release
> candidates. See the [roadmap](ROADMAP.md).

## Goals

- **Native and fast.** egui on the GPU; the terminal grid has its own wgpu
  renderer, glyphs drawn once into an atlas.
- **Built for network gear.** Serial with BREAK, Telnet, legacy SSH algorithms
  per session, SecureCRT session import.
- **Everything is a command.** The GUI, the `przm` CLI, scripts and an MCP server
  call the same command bus.
- **Plain-text configuration.** Sessions are TOML files you can keep in Git.
  Passwords live in the OS keychain, never in the files.

## Install

Download the file for your system from the
[releases](https://github.com/rodrigoprazim/przmterm/releases). To remove
PrzmTerm, delete what you installed, and the settings folder if you no
longer want your sessions (see
[Sessions and folders](docs/GUIDE.md#sessions-and-folders)).

- **Windows:** unpack the `.zip` and run `przmterm.exe` (the desktop app)
  or `przm.exe` (the command line). The executables are not signed yet (see
  [Code signing policy](#code-signing-policy)), so the first time Windows
  shows *Windows protected your PC*: choose *More info › Run anyway*.
- **macOS:** open the `.dmg` and drag PrzmTerm to Applications. The app is
  universal (Apple Silicon and Intel) but not notarised, which needs a paid
  Apple account, so the first time macOS refuses to open it: go to
  *System Settings › Privacy & Security* and choose *Open Anyway* (once).
  When macOS asks whether PrzmTerm may find devices on your local network,
  allow it, or hosts on your network cannot be reached. The command line is
  inside the app; to type `przm` in Terminal:
  `sudo ln -s /Applications/PrzmTerm.app/Contents/MacOS/przm /usr/local/bin/przm`.
- **Linux:** unpack the `.tar.gz`; x86_64, needs a desktop with OpenGL or
  Vulkan.

*Check for Updates…* (the PrzmTerm menu on macOS, the **+** menu elsewhere)
looks for a newer release on GitHub; PrzmTerm also looks when it starts and
shows a newer version in the status bar. It only points to the download
page: nothing is installed for you.

`SHA256SUMS.txt` lists every file's checksum. When it comes with
`SHA256SUMS.txt.asc`, that is signed with the release key in
[docs/release-key.asc](docs/release-key.asc), fingerprint
`7DD0 DEC1 0FB0 F51E E1AD 6455 96A1 88FD 92F2 8ECF`:

```sh
gpg --import docs/release-key.asc
gpg --verify SHA256SUMS.txt.asc SHA256SUMS.txt
sha256sum --check --ignore-missing SHA256SUMS.txt
```

## Build

Requires Rust 1.95 or newer.

```sh
git clone https://github.com/rodrigoprazim/przmterm
cd przmterm
cargo run --release            # desktop app (przmterm)
cargo run --release -p przmterm-cli -- --help   # CLI (przm)
```

The terminal grid is drawn with wgpu. If text looks wrong on a graphics
driver, `PRZMTERM_RENDERER=egui` paints it with egui instead, as before
0.9.0.

## Using PrzmTerm

Type `user@host:port` in **Quick connect**, or create sessions with **+**
next to *Sessions* and double-click one to connect. Sessions live in folders
that pass their settings down (user name, keyword set, logging…), and every
setting is a plain TOML file you can edit, keep in Git or sync between
computers. Passwords go to the system keychain or to an encrypted vault,
never to those files.

- **[User guide](docs/GUIDE.md):** sessions and folders, connecting, the
  password vault, serial and Telnet, tunnels and jump hosts, splits and tab
  groups, buttons, shortcuts and triggers, keyword highlighting, logging,
  SFTP, SCP and Zmodem, `przm run`, AI assistants (MCP), backups and moving
  from SecureCRT.
- **[Configuration reference](docs/CONFIG.md):** every file and setting, and
  what stays compatible from 1.0 on.
- **[Scripting reference](docs/SCRIPTING.md):** everything a Rhai script can
  do, with examples.

| Action | Linux / Windows | macOS |
| --- | --- | --- |
| New tab (same session, no new login) | Ctrl+Shift+T | ⌘⇧T |
| New PrzmTerm tab (to connect to another host) | Ctrl+Shift+N | ⌘⇧N |
| Split: grid / side by side / stacked | Ctrl+Shift+G / D / E | ⌘⇧G / D / E |
| Copy / paste | Ctrl+Shift+C / Ctrl+Shift+V | ⌘C / ⌘V |
| Hide / show the session list | Ctrl+Shift+S | ⌘⇧S |

A sample configuration is in [examples/config](examples/config). Try it with
`PRZMTERM_HOME=examples/config cargo run`.

## Workspace

| Crate | Role |
| --- | --- |
| `przmterm-core` | Command bus, shared types |
| `przmterm-transport` | `Transport` trait; SSH, Telnet, serial and local implementations |
| `przmterm-term` | Escape-sequence parser, grid, scrollback |
| `przmterm-session` | Session files and inheritance |
| `przmterm-script` | Rhai scripts that drive a session |
| `przmterm-connect` | A session's transport: jump hosts, vault entries, saved passwords |
| `przmterm-cli` | The `przm` binary |
| `przmterm-app` | The `przmterm` desktop binary |

## Code signing policy

Releases are built from this repository's source by the
[release workflow](.github/workflows/release.yml) on GitHub Actions. Their
checksums are signed with the release key (see [Install](#install)). Windows
executables are not signed yet: the free code signing of the
[SignPath Foundation](https://signpath.org) is for projects with a wider
following, and PrzmTerm will apply again once it has one. The workflow is
ready to sign through SignPath, each signing approved by hand.

| Role | Who |
| --- | --- |
| Committers and reviewers | [Rodrigo Prazim](https://github.com/rodrigoprazim) |
| Approvers | [Rodrigo Prazim](https://github.com/rodrigoprazim) |

**Privacy:** PrzmTerm does not send any information to other networked
systems unless the user asks for it: it connects to the hosts you open and,
when it starts, asks GitHub for the list of PrzmTerm releases to tell you of
a newer one (turn that off in *Check for Updates…*). No telemetry.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). PrzmTerm is a clean-room project: it
reproduces workflows from public documentation and everyday use, never from
another vendor's code or assets.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT) at your option.
