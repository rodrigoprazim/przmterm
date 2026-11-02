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

### Moving from SecureCRT

**+** next to *Sessions* › *Import from SecureCRT…* takes SecureCRT's XML
export (*File › Export Settings*) or session `.ini` files; *Import a SecureCRT
Sessions folder…* takes the whole `Config/Sessions` folder (macOS:
`~/Library/Application Support/VanDyke/SecureCRT/Config/Sessions`). Sessions
land in a `SecureCRT` folder with the same sub-folders; names already used get
` (2)`. From the command line:

```sh
przm import-securecrt --dry-run export.xml     # show what would be imported
przm import-securecrt export.xml --into SecureCRT
```

Passwords are not imported: SecureCRT keeps them encrypted, and PrzmTerm asks
at the first login and can save them in the system keychain. Logon scripts,
port forwards and RDP or local-shell sessions are listed in the summary.

### Password vault

**+** next to *Sessions* › *Password vault…* creates an encrypted vault of
user names, passwords and key passphrases, opened with a master password
(it cannot be recovered, so keep it safe). Give a session or a whole folder
an entry under *Vault entry* in its settings: its user name replaces the
session's, its password is used before the keychain's, and its passphrase
opens encrypted key files. When the password of many devices changes, edit
one entry.

The vault is `<config>/vault.toml`. It can be copied to another machine and
opened there with the same master password; backups include it, still
encrypted. Opening a session that uses the vault asks for the master
password once per run; *Lock* in the vault window asks again next time.
Sessions without an entry keep using the system keychain.

### Backup and sharing sessions

**+** next to *Sessions* › *Back up settings…* writes one `.przm` file with
everything: global settings, folders, sessions, keyword sets, buttons, known
hosts and the password vault (still encrypted). *Restore or import…* takes it
back, on this or another machine; files with the same name are replaced, and
the settings in use are first saved in `<config>/backups/`.

To share sessions, right-click a folder or session › *Export folder…* or
*Export session…*. Whoever gets the file picks *Restore or import…* and the
sessions arrive in a new folder; nothing of theirs is replaced. Passwords are
never in these files (they stay in the system keychain), but triggers are
copied as typed, so check them before sharing if they answer password
prompts.

### Buttons and sending to several sessions

**▤ Buttons** in the status bar shows the button bar: tabs of buttons (one per
device type, say) that send commands. **+** on the bar adds a button;
right-click a button to edit, move or delete it, and a tab to rename, add or
delete tabs (or turn tooltips off). A button sends to the current session. In
the text, `\r` is Enter, `\t` Tab, `\e` Esc, `\n` a line feed and `\p` a
one-second pause. Buttons are kept in `<config>/buttons.toml`:

```toml
[[tab]]
name = "Cisco"

[[tab.button]]
label = "Save"
send = 'copy run start\r\p\r'   # single quotes keep the backslashes
description = "Saves the running configuration"
```

**◫ Split ▾** at the top right joins sessions open in other tabs to the
current one: *Grid (two per row)*, *Side by side* or *Stacked*, then tick the
sessions (several at once); they move into this tab. To see one session
twice, clone it first (*Clone Session* in the tab menu). In a split tab the
same menu rearranges the panes, moves the active pane to a tab of its own or
separates them all into tabs again. Each pane shows its session's name, and
connected sessions have a green dot in the session list.

In a split tab, **⇶ Send to all** in the status bar sends what you type to
every pane of the tab (after a warning) until you turn it off.

### Triggers

A trigger types an answer when some text shows up: `--More--` → space,
`[confirm]` → Enter, a second device's prompt → a command. Set them under
**Triggers** in the session, folder or global settings, or in the file:

```toml
[[trigger]]
match = "--More--"
send = ' '

[[trigger]]
match = '\[confirm\]$'     # a regular expression
regex = true
send = '\r'
once = true                # only the first time per connection
```

Triggers are saved as plain text, so leave passwords to the keychain.

### Scripts

Scripts are [Rhai](https://rhai.rs) files (`.rhai`) kept in
`<config>/scripts/`. Run one with *Run Script…* in the tab menu, put it on a
button (*Script* in the button dialog), or give a session a **Logon script**
that runs each time it connects. **■ Stop** in the status bar stops it.

```rhai
// Log in to a switch that asks for its own user name and password.
if session.wait_for("Username:", 15) {
    session.send(session.username + "\r");
    session.wait_for("Password:");
    session.send(prompt_secret("Password for " + session.name) + "\r");
}
let seen = session.wait_for(["#", ">", "denied"], 10);   // index, or -1
if seen == 0 {
    session.send("terminal length 0\r");
    session.wait_for("#");
    session.send("show version\r");
    let version = session.read_until("#", 10);   // the output before "#"
    log("version: " + version.len() + " characters");
}
```

| In a script | Does |
| --- | --- |
| `session.send(text)` | types `text` (`"\r"` is Enter) |
| `session.wait_for(text)` / `(text, seconds)` | `true` once `text` shows up; `false` if the session closes or time runs out |
| `session.wait_for([a, b, …], seconds)` | index of the text seen first, `-1` if none |
| `session.read_until(text, seconds)` | the output before `text`; an error on time-out |
| `session.name`, `.host`, `.username` | the session's details |
| `log(text)`, `print(text)` | shows a line in the terminal (not sent) |
| `prompt(question)`, `prompt_secret(question)` | asks you; *Stop script* cancels |
| `sleep(ms)` | waits |

Waits see the output as plain text (colours and cursor moves removed), and
each match consumes the output up to it, so the same prompt is not matched
twice.

**+** › *Script guide…* explains all of this in the app, with a snippet per
topic and ready examples ([`examples/scripts/`](examples/scripts/)) that can
be edited, checked and saved as your own scripts.

### Keyword highlighting

A keyword set colours words and patterns in the output: `up` green, `down`
red, IP addresses, MAC addresses… Choose one under **Keywords** in the
session, folder or global settings (`keywords = "CORES"`). **+** ›
*Keyword sets…* (or *Edit sets…* next to *Keywords*) opens the editor, with a
live preview. Sets are files in `<config>/keywords/<name>.toml`:

```toml
[[rule]]
text = "down"                 # whole word: also "link-down", not "downtime"
color = "#ff0000"

[[rule]]
text = "admin-down"           # longer matches win over "down"
color = "#ffff00"

[[rule]]
regex = '\b\d{1,3}(\.\d{1,3}){3}\b'
color = "#ff55ff"
bold = true
```

SecureCRT keyword lists (`Config/Keywords/*.ini`) come in with
*Import from SecureCRT…*. Only text the remote side left uncoloured is
recoloured, so `ls --color` or `vim` keep their own colours.

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

### X11 forwarding

**X11 forwarding** (`x11_forwarding = true`) works like `ssh -X`: programs
started on the server (`xclock`, `wireshark`, installers) open on this
computer. It needs a local X server: XQuartz on macOS (log out and in once
after installing it), VcXsrv or similar on Windows; Linux desktops have one.
PrzmTerm uses `$DISPLAY` (default `:0`, or `localhost:0` on Windows) and the
display's cookie from `xauth`; the server only ever sees a random stand-in.

### Old equipment

Switches and routers that only know old SSH algorithms connect with
**Legacy algorithms** on (`legacy_algorithms = true`): SHA-1 key exchange,
CBC and 3DES ciphers, HMAC-SHA1 and DSA host keys are then offered after the
modern ones. Such a connection shows a **legacy** badge in the status bar and
⚠ on its tab. Without the option, the error names the algorithms the server
offers.

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
| Split: grid / side by side / stacked | Ctrl+Shift+G / D / E | ⌘⇧G / D / E |
| Close pane | Ctrl+Shift+W | ⌘⇧W |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab | same |
| Copy | Ctrl+Shift+C, or Ctrl+C with a selection | ⌘C |
| Paste | Ctrl+Shift+V or Ctrl+V | ⌘V |
| Scroll back | wheel, Shift+PageUp/PageDown, Shift+Home/End | same |
| Send BREAK (serial, Telnet) | Ctrl+Shift+B | ⌘⇧B |
| Hide / show the session list | Ctrl+Shift+S | ⌘⇧S |

Drag to select, double-click selects a word, triple-click a line. Right-click
opens Copy / Paste / Select all.

These are the built-in shortcuts. **+** › *Keyboard shortcuts…* changes them
and adds others: a key can send text (`show ip interface brief\r`), run an app
command, run a script or press a button. They are kept in
`<config>/keymap.toml`:

```toml
[[key]]
keys = "F5"                    # Cmd is ⌘ on macOS and Ctrl elsewhere
send = 'show ip interface brief\r'

[[key]]
keys = "Alt+1"
script = "cisco-show.rhai"

[[key]]
keys = "Cmd+Shift+T"
command = "new_tab"
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
identity_file = "~/.ssh/id_lab"
save_password = true       # keychain: unset asks, true always, false never
debug = true               # show each step of the SSH login (like ssh -v)
keepalive = 30             # seconds between keepalives, 0 = off
auto_reconnect = true      # reconnect when the connection drops
legacy_algorithms = true   # also offer old SSH algorithms (old switches)
x11_forwarding = true      # remote graphical programs on this screen (ssh -X)
keywords = "CORES"         # keyword set for highlighting (<config>/keywords/)
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
| `przmterm-script` | Rhai scripts that drive a session |
| `przmterm-cli` | The `przm` binary |
| `przmterm-app` | The `przmterm` desktop binary |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). PrzmTerm is a clean-room project: it
reproduces workflows from public documentation and everyday use, never from
another vendor's code or assets.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT) at your option.
