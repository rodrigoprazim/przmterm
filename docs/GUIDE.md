# PrzmTerm user guide

How to use PrzmTerm, task by task. Two references go with it: the
[configuration reference](CONFIG.md), with every setting and file, and the
[scripting reference](SCRIPTING.md), with everything a script can do.

- [First steps](#first-steps)
- [Sessions and folders](#sessions-and-folders)
- [Connecting](#connecting)
- [Passwords: keychain and vault](#passwords-keychain-and-vault)
- [Network equipment](#network-equipment)
- [Tunnels, jump hosts and X11](#tunnels-jump-hosts-and-x11)
- [Working in the terminal](#working-in-the-terminal)
- [Tabs, splits and windows](#tabs-splits-and-windows)
- [Buttons, shortcuts and triggers](#buttons-shortcuts-and-triggers)
- [Keyword highlighting](#keyword-highlighting)
- [Logging](#logging)
- [Files: SFTP, SCP, Zmodem](#files-sftp-scp-zmodem)
- [Scripts](#scripts)
- [Commands without a window](#commands-without-a-window)
- [AI assistants (MCP)](#ai-assistants-mcp)
- [Backup, sharing and sync](#backup-sharing-and-sync)
- [Moving from SecureCRT](#moving-from-securecrt)
- [When something goes wrong](#when-something-goes-wrong)

## First steps

The window has the **session list** on the left, the **tabs** on the right
and the **status bar** at the bottom. **Quick connect** at the top takes
`user@host:port` (also `telnet://host` and `serial://PORT@BAUD`), and a double
click on a saved session opens it.

The first tab is PrzmTerm's own small shell, not a system one: `ssh
router-lab` opens a saved session (Tab completes the name, or `Lab/Core/` for
a folder), `ssh -p 2222 admin@10.0.0.1` a host that is not saved, `telnet host
[port]` a Telnet one, and `sessions [text]` lists them. Up and Down go
through what was typed; `help` lists the commands. It also adds, deletes
and exports saved sessions; see [Sessions from the PrzmTerm shell](#sessions-from-the-przmterm-shell). Open another with **+**
at the end of the tabs, a double click on the free part of the tab bar, or
Ctrl+Shift+N (⌘⇧N on macOS).

A session's or folder's *Description* (in its settings) shows when the
pointer rests on it in the session list, and the search above the list finds
it too.

The **+** above the session list holds the app's windows: password vault,
keyword sets, keyboard shortcuts, script guide, appearance, settings folder,
backups and imports. On macOS they are also in the PrzmTerm menu, and
*Help* has the script guide.

## Sessions and folders

A session is a file, and a folder of sessions is a directory, in the
**settings folder**: `$PRZMTERM_HOME` when it is set, the folder chosen in
**+** › *Settings folder…* (see [Several computers](#several-computers)), or
by default:

| OS | Directory |
| --- | --- |
| Linux | `~/.config/przmterm` |
| macOS | `~/Library/Application Support/PrzmTerm` |
| Windows | `%APPDATA%\PrzmTerm` |

```text
przmterm/
  defaults.toml           # global defaults
  sessions/
    core-sw-01.toml
    Cisco/
      folder.toml         # inherited by everything in Cisco/
      edge-rtr-01.toml
```

Each session takes what it does not set from its folder, the folders above,
and then the **global settings**. Set something once on a folder (a user
name, a keyword set, a log) and every session in it follows. Every key is in
the [configuration reference](CONFIG.md).

A session that is open takes its changed settings as soon as they are
saved (in the session, its folders or the global settings): the font and
colours, history size, keyword set, triggers, anti-idle, SCP shell,
logon script and auto-reconnect. A log that is running keeps its file, so
nothing is split off: a new log folder or name is used once the log is
stopped and started again (or at the next connection). Settings of the connection itself (host,
port, user, protocol, credential, key, jump host, tunnels, X11, keepalive,
terminal type) are used when it reconnects; the message after saving
names the sessions this applies to.

Sessions can also be created and edited from the app: **+** next to
*Sessions*, or right-click a session for Edit, Duplicate and Delete. Right-click
a folder to add a session or subfolder there, rename it, give it a colour
(*Color*, also in its settings), edit the settings its sessions inherit, or
delete it. Right-click the free space below the list for a new session or
folder at the top level, *Expand all*, *Collapse all*, *Reload*, the
global settings and the settings folder. Deleting a folder can keep what is in
it: its sessions and subfolders move up a level and keep the folder's
settings (written into each), so they connect as before.

The *Folder* list in a session's settings shows every folder as a tree, with
a search (Enter takes the first folder found). *New folder in …* at its
bottom names a folder inside the one selected; it is made only when the
session is saved, so Cancel leaves nothing behind.

When a settings file has a problem (a session without a host, a typo in a
file edited by hand, a setting from a newer version), a warning under the
session list says how many. Click it for the list: each file, what is
wrong, and what can be done: *Edit…* a session that is not listed until it
is fixed, *Remove them* for unknown settings, *Open* the file, *Show in
folder* or *Delete* it; *Reload* reads the files again after fixing one by
hand.

Global defaults live in `defaults.toml` (⚙ next to *Sessions* in the app).
Changing them asks which existing sessions take the new values: those that
use the default (sessions with their own value keep it), all of them
(their own values for those settings are removed), or only new sessions
(PrzmTerm writes the old value into the existing ones).

Unlock the padlock next to *Sessions* to drag sessions and folders (by the
dots before their names) onto another folder, or to the top level.

A sample tree is in [examples/config](../examples/config). Try it with
`PRZMTERM_HOME=examples/config cargo run`.

## Connecting

Open a saved session with a double click (or *Connect* in its right-click
menu), or type the address in Quick connect. Host key questions, `login as:`, passwords, key passphrases and two-factor
prompts appear in the terminal, as with OpenSSH. Authentication tries, in
order: ssh-agent (Pageant or the OpenSSH agent on Windows), the session's
`identity_file`, `~/.ssh/id_ed25519`, `id_ecdsa`, `id_rsa`, keyboard-interactive
and password.

Passwords are never written to session files. After a typed password works,
PrzmTerm offers to save it in the OS keychain (Keychain on macOS, Credential
Manager on Windows, Secret Service on Linux); right-click a session to forget it.
On macOS, builds not signed with an Apple Developer ID (development builds
and the releases) make the system ask for your login keychain password (your
Mac account password) after each rebuild or update; choose *Always Allow*.

### Why did the login fail?

Turn on **Debug login** in the session editor (or `debug = true` in the
session file), or type `-v` before the target in Quick connect
(`-v admin@10.0.0.1`). Each step then shows in the terminal in cyan, like
`ssh -v`: the addresses tried, the server version and algorithms, the host key
and the known_hosts file it was found in, every key, password and
keyboard-interactive attempt with the server's answer and the methods it still
allows, and the reason the server gave if it disconnected.

### Keepalive, reconnect and anti-idle

Sessions send a keepalive every 30 s (`keepalive`, 0 turns it off). With
**Auto reconnect** on (`auto_reconnect = true`), a pane whose connection drops,
or cannot be made, tries again after 2, 5, 10, 20 and then every 30 seconds,
on the same screen. Enter reconnects at once; Ctrl+C stops the retries. A
normal logout or a rejected login never triggers it.

Some devices close a session when nobody types, whatever the keepalives.
**Anti-idle** (*Automation* page of the editor) sends a text after that many
seconds without typing: one character, sent as it is with no Enter after it
(`anti_idle = ' '`), with `anti_idle_interval = 60` (the default;
0 turns it off). It keeps going while the window is minimized or hidden,
waits until an SSH login is over, and applies to quick connects when set in
the global settings; a change reaches open sessions at once. SecureCRT's
"Send string" setting is imported.

### Host keys

Accepted host keys go to `known_hosts` in the configuration directory (OpenSSH
format). `~/.ssh/known_hosts` is also consulted, read-only. If a host's key
changes, PrzmTerm refuses to connect and shows the old and new fingerprints.

## Passwords: keychain and vault

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

Buttons and shortcuts can type a vault entry's password too: see
[Buttons](#buttons).

## Network equipment

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

### Old equipment

Switches and routers that only know old SSH algorithms connect with
**Legacy algorithms** on (`legacy_algorithms = true`): SHA-1 key exchange,
CBC and 3DES ciphers, HMAC-SHA1 and DSA host keys are then offered after the
modern ones. Such a connection shows a **legacy** badge in the status bar and
⚠ on its tab. Without the option, the error names the algorithms the server
offers.

## Tunnels, jump hosts and X11

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

## Working in the terminal

| Action | Linux / Windows | macOS |
| --- | --- | --- |
| New tab (same session, no new login) | Ctrl+Shift+T | ⌘⇧T |
| New PrzmTerm tab (to connect to another host) | Ctrl+Shift+N | ⌘⇧N |
| Split: grid / side by side / stacked | Ctrl+Shift+G / D / E | ⌘⇧G / D / E |
| Close pane | Ctrl+Shift+W | ⌘⇧W |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab | same |
| Copy | Ctrl+Shift+C, or Ctrl+C with a selection | ⌘C |
| Paste | Ctrl+Shift+V or Ctrl+V | ⌘V |
| Scroll back | wheel, scroll bar, Shift+PageUp/PageDown, Shift+Home/End | same |
| Send BREAK (serial, Telnet) | Ctrl+Shift+B | ⌘⇧B |
| Hide / show the session list | Ctrl+Shift+S | ⌘⇧S |
| Zoom the pane's font in / out | Ctrl+wheel, Ctrl+= / Ctrl+- | ⌘+wheel or pinch, ⌘= / ⌘- |
| Reset the zoom | Ctrl+0, or ⟲ in the status bar | ⌘0, or ⟲ |
| Find in the session | Ctrl+Shift+F, or the magnifier in the status bar | ⌘F |
| Global settings (of the sessions) | the gear above the sessions | same |
| PrzmTerm's settings (look, mouse) | **+** › *Settings…* | ⌘, |

Zooming changes the font of the pane under the pointer (the keys: the
active pane), a point at a time, and is not saved. While a pane is zoomed,
the status bar shows its size and the reset arrow, which goes back to the
size it had. To keep a size, set it in *Font, Colors and Cursor…*.

Drag to select, double-click selects a word, triple-click a line. Right-click
opens Copy / Paste, Find and Highlight (the text selected, when there is
one) and Select all. Text with line breaks is shown before it is pasted, to
check or edit it.

**+** › *Settings…* (*PrzmTerm* › *Settings…* on macOS) has the *Mouse*
options, for the whole app, all off until chosen:

- *Copy on select*: what is selected is copied at once.
- *Paste on* the middle or the right button. With the right one the
  terminal has no right-click menu.
- *Hide the mouse pointer while typing*, until it moves.
- *Open web addresses with Ctrl+click* (⌘+click): the pointer turns into a
  hand over an address while Ctrl (⌘) is held.
- *Triple-click selects the line break too*.

*Backspace sends*, on the *Terminal* page of the session settings, is `^H`
(BS), as in SecureCRT: shells erase with it, and Huawei VRP and Nokia
equipment need it (they read `^?` as "delete the character under the
cursor"). `^?` (DEL, what Linux terminals send) is for the rare program that
erases only with it, such as `cat` or a script's `read`, which show `^H`.

*Font, Colors and Cursor…* in a tab's menu picks the font (the built-in one or
any installed monospace font), its size, the terminal's background and text
colours, the cursor (block, underline or bar), its colour and whether it blinks;
*Save* keeps them for the session. They are also in the session settings
(*Terminal* page), so a folder or the defaults can set them for many sessions.

*Settings…* also switches the interface between dark and light, or follows the system's mode. The terminal
keeps its own colours whichever is chosen.

On macOS the PrzmTerm menu in the menu bar has *About PrzmTerm*, *Settings…*
(PrzmTerm's own) and the other app windows (shortcuts, vault, settings
folder), without icons,
and *Help* has the script guide and the project's page. Elsewhere they are in
the **+** menu above the sessions.

## Tabs, splits and windows

The split icons at the top right join sessions open in other tabs to the
current one: *Split Right* (side by side) and *Split Down* (stacked), and
**⋯** for *Grid (two per row)*; then tick the sessions (several at once);
they move into this tab. To see one session twice, clone it first (*Clone
Session* in the tab menu). In a split tab the **⋯** menu rearranges the
panes, moves the active pane to a tab of its own or
separates them all into tabs again. Each pane shows its session's name, and
connected sessions have a green icon in the session list (a terminal for
SSH, a screen for Telnet, a cable for serial, a laptop for a local shell).

A tab of a connection starts with an icon: a green plug while connected, a
red unplugged one once the connection has ended. A tab you are not looking
at tells you when output comes: a green pulse while it is coming in, then a
green bell once it stops (the command you left running has finished, say)
until you open the tab. Split panes show the same icon in their caption.

In a split tab, **⇶ Send to all** in the status bar sends what you type to
every pane of the tab (after a warning) until you turn it off.

In a split tab, drag a pane by its caption onto another to swap them, or
right-click the caption to move it left or right.

Drag a tab along the tab bar to move it; a line shows where it will go.
Dropped among the tabs of a group it joins the group; dragged away from
its group it leaves it.

**Tab groups.** *Group › New Group* in a tab's menu starts a group: its tabs
sit together after a coloured label, each with a bar of its colour, and *Group › Add to …* puts more tabs in
it. Clicking the label offers *Send Command to Group…* (one or more commands,
to every connected session of the group, with Enter after each line),
*Rename Group…*, *Ungroup* and *Close Group*.

**Windows.** *Window › Move to New Window* opens the tab in a window of its
own, the connection and all; *Clone to New Window* opens a second connection
to the same session there; *Move to Main Window* brings it back. Other
windows have their tabs and panes; dialogs and the session list stay in the
main one. Closing a window closes its tabs (asking first if any is
connected).

**Locking.** *Lock…* hides a tab's screen behind a password: nothing is typed
into it, it does not close, and its session cannot be cloned or browsed until
the password is given again. The connection stays up and keeps receiving.
The password is only kept in memory (as an Argon2 hash).

## Buttons, shortcuts and triggers

### Buttons

**▤ Buttons** in the status bar shows the button bar: groups of buttons (one
per device type, say) that send commands. The list at the left of the bar
picks the group; **⋯** next to it renames,
adds or deletes groups, or shows or hides all the group's tooltips. **+** on
the bar adds a button; right-click a button to edit, move or delete it (or
turn its tooltip off). A button sends to the current session. In
the text, `\r` is Enter, `\t` Tab, `\e` Esc, `\n` a line feed and `\p` a
one-second pause. **Vault ▾** next to the text adds `{vault:NAME}`, which
types the password of that [vault](#passwords-keychain-and-vault) entry
(`{vault:NAME:username}` its user name): the button keeps only the entry's
name, and the vault is unlocked first if it is locked. Shortcuts that send
text take the same.

**Only when** keeps a button from typing at the wrong place: a regular
expression for the prompt the button answers, matched at the end of the
line, right before the cursor (trailing spaces aside). `(?i)password:|senha:`
suits a button that types a password; `]#` matches `[root@host ~]#`, while
`]` alone does not, since that line ends with `#`. `.*` reaches further
back: `(?i)password.*:` also matches `[sudo] password for root:`. Pressed
anywhere else, the button sends nothing and the status bar says so, and the
vault is not even asked for. The check is made when the button is pressed;
`(?i)` makes it ignore case. The dialog warns when a button types from the
vault without it.

Changing a button or shortcut that types from the vault asks for the master
password, even while the vault is open: opening it to edit, or saving one
that newly types from the vault. Pressing it never asks (beyond unlocking a
locked vault). This guards the app, not the file: anyone who can edit
`buttons.toml` can still change it there. Buttons are kept in
`<config>/buttons.toml`:

```toml
[[group]]
name = "Cisco"

[[group.button]]
label = "Save"
send = 'copy run start\r\p\r'   # single quotes keep the backslashes
description = "Saves the running configuration"
```

### Keyboard shortcuts

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

## Keyword highlighting

A keyword set colours words and patterns in the output: `up` green, `down`
red, IP addresses, MAC addresses… It is optional: none is made unless you
make one. The *Keywords* page of the session, folder or global settings has
both: *Keyword set* at the top is the one this session uses (`keywords =
"CORES"`; a set that is gone shows as *not found*), and *Keyword sets* below
edits the sets, with a live preview. *Editing* picks the set to edit (which
does not choose it for the session) and says which sessions use it; *New
set* makes one; changes are saved with the settings. *Delete…* asks first;
deleting a set takes it out of the sessions and folders that chose it, and
renaming one renames it there. Sets are files in
`<config>/keywords/<name>.toml`:

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
*Import from SecureCRT…*, and open sessions that use them highlight with
them at once. Only text the remote side left uncoloured is
recoloured, so `ls --color` or `vim` keep their own colours.

### Highlights of one session

For troubleshooting, mark text in one session only, while it is open:
**Highlights** in the status bar, or select the text, right-click ›
*Highlight "…"*. Each highlight has its text (found anywhere in a line;
with **.\*** a regular expression, with **Aa** matching case), a colour, and
*Text* (only the text) or *Line* (the whole line). They are painted over the
program's own colours, show as marks on the scroll bar, and change as you
type. With the bell on, the tab shows a marker in the highlight's colour
when the text turns up while you are on another tab, so a `tail -f` can run
in the background. Nothing is saved: *Save as keyword set…* keeps them as a
keyword set, to choose in other sessions.

### Finding text

Ctrl+Shift+F (⌘F on macOS), the magnifier in the status bar or
right-click › *Find* opens the find bar. It searches the session's history
and screen as you type, marks every match (also on the scroll bar) and
counts them. Enter goes to the older match, Shift+Enter to the newer; the
count follows new output. **Aa** matches case, **.\*** takes a regular
expression, and the marker button turns the search into a highlight. In a
split tab the grid button searches every pane instead of only the one with
the keyboard. Esc closes the bar.

## Logging

**Log** in the status bar starts and stops logging the current pane to a
file; *Start / stop the log* is also a shortcut command. With **Log to file**
on in a session's (or a folder's, or the global) *Logging* page, every
connection of those sessions is logged from the start.

The file name can hold the session's name, the host, the date and the time
(`%S-%Y%M%D-%h%m%s.log` by default). A relative name goes in the *Folder*
(`<config>/logs` by default). The log holds the text as shown, without
colours or escape sequences. The codes are listed in the
[configuration reference](CONFIG.md#log).

Right-click **Log** for more:

- *Log to File…* logs the current pane to a file you name, anywhere,
  from now on. It is for this pane only and is forgotten when it closes;
  the session's own log settings do not change. Stopping and starting the
  log again adds to the same file.
- *Save History As…* writes what the pane's history and screen hold now
  to a text file.

## Files: SFTP, SCP, Zmodem

### SFTP

*File Browser › SFTP* in a tab's menu, or in an SSH session's menu in the
list (it connects first if needed), opens a file browser on the same connection, without
logging in again. Double-click a folder to open it and a file to download
it; *Download…* takes several files or whole folders. *Upload files…*,
*Upload folder…* or dropping files on the window sends them to the folder
shown. Transfers run in the background with a progress bar and *Cancel*.
Right-click a file or folder for its menu: *Open* (folders), *Download…*,
*Rename…*, *Delete…*, *Copy Path* and *Copy Name*, and the folder's *New
Folder…*, *Upload Files…*, *Upload Folder…* and *Refresh*. It acts on the
selection; right-clicking something not selected selects it first. The
empty part of the list has the folder's menu, with *Copy Folder Path*.

Click a column's header to sort by it (name, size, date or permissions),
again to reverse; folders stay on top. Drag the line between two headers
to resize the columns; double-click it for the first widths. The window
opens in the middle and keeps the size you give it.

### SCP, also as root

*File Browser › SCP* (same menus) opens the same file browser as SFTP,
over SCP on the same connection: folders are listed with `ls` and changed
with `mkdir`, `rm` and `mv`, and files move with `scp`, as in WinSCP's SCP
mode. The commands run in the session's **SCP shell** (session settings,
*Shells ▾*: *Default* is the login shell; `/bin/bash`, `/bin/ksh`, or
`sudo su -` and `su -` to work as root — the same as typing `sudo su -`
first). When the shell asks for a password, the browser asks for it once.
If the shell cannot start (`sudo su -` on a host without sudo, say), the
browser says why and offers *Open without the SCP shell*: it then works as
the user logged in, with that user's permissions, and says so at its top;
closing the window gives up.
**⇅ Transfer** › protocol *SCP* sends or fetches files the same way, beside
the terminal, with the progress in the status bar. The other side needs
`scp`; with the *Default* shell nothing else is typed, so routers and
switches with an SCP server work too.

### Xmodem, Ymodem and Zmodem

For serial consoles and hosts without SFTP. Run `sz file` on a Unix host and
the file arrives in your Downloads folder; run `rz` and PrzmTerm asks which
files to send. `rz` and `sz` come from the lrzsz package (`dnf install
lrzsz`, `apt install lrzsz`); without it, use SFTP. Routers, switches and
OLTs do not have it. For a device (a bootloader waiting for firmware, say) use
**⇅ Transfer** in the status bar (tabs *Send* and *Receive*): choose the protocol and
the files, with *Start the other side for me* off. The status bar shows the
progress and *Cancel*. Zmodem streams and is much faster over a network;
Xmodem and Ymodem wait for an answer after each block.

## Scripts

Scripts are [Rhai](https://rhai.rs) files (`.rhai`) that drive a session:
they type commands, wait for the answers, read them and react. They are kept
in `<config>/scripts/`. Run one with *Run Script…* in the tab menu, put it on a
button or a shortcut, or give a session a **Logon script** that runs each time
it connects. **■ Stop** in the status bar stops it.

```rhai
session.send("terminal length 0\r");
session.wait_for("#", 10);
session.send("show version\r");
session.wait_for("show version", 10);
let version = session.read_until("#", 30);   // the output before "#"
log("version: " + version.len() + " characters");
```

**+** › *Script guide…* teaches scripting in the app, with a snippet per
topic and ready examples ([`examples/scripts/`](../examples/scripts/)) that
can be edited, checked and saved as your own. Every function, how waiting
works, errors and the examples are in the
[scripting reference](SCRIPTING.md).

## Commands without a window

`przm run` connects a session from a shell or a cron job, logs in, sends
commands and prints what comes back:

```sh
przm run core-sw-01 -c 'show clock' -c 'show version'
przm run Lab/Core/router-lab < commands.txt     # one command per line
przm run admin@10.0.0.1:2222 --password-env PW -c uptime
```

The session logs in as it does in the app: with its key, its vault entry
(the master password comes from `PRZMTERM_VAULT_PASSWORD`, or is asked) or
the password saved in the keychain. Other login questions are asked on the
terminal; without one, the run stops and says what was asked. A host key seen
for the first time needs `--accept-host-key`. Each command waits for the
prompt (`--prompt`, a regular expression, by default a line ending in `$ # >
% ]`) up to `--timeout` seconds; the session's triggers answer `--More--` and
the like. The exit code is 0 when all commands ran, 1 when the connection or
login failed and 2 when a prompt did not come back. `-v` shows the login on
stderr, `--raw` keeps the escape sequences.

The saved sessions can be managed from a shell too, with the same rules as
the [PrzmTerm shell's commands](#sessions-from-the-przmterm-shell):

```sh
przm add "Core DB" admin@10.0.0.5:2222 -f "Data Center" -d "Main database"
przm add sw-core-01 10.0.0.1 -l netops -f "Data Center/Network" -c netops
przm delete sw-core-01 -f "Data Center/Network"
przm delete -f Lab --yes                        # a folder and what is in it
przm export > hosts.txt                         # on the old computer
przm import hosts.txt                           # on the new one (or: < hosts.txt)
przm sessions                                   # the tree, with descriptions
```

`delete` asks first; in a script add `--yes`. For `-c`, the vault is
opened (master password from `PRZMTERM_VAULT_PASSWORD`, or asked) only when
the entry is not used by a session yet. `import` adds every `add` line of
the list (the ones `export` prints, also pasted into a PrzmTerm shell),
skipping blank lines and `#` comments; it goes on past a line it cannot
add, says which, and exits with 1. `--replace` writes over sessions of the
same names.

Tab completes `przm`'s commands and options, saved sessions (`przm run`,
`przm delete`, `-J`), folders a level at a time (`-f`) and the vault
entries the sessions use (`-c`). Turn it on once in the shell's startup
file:

```sh
echo 'source <(COMPLETE=zsh przm)' >> ~/.zshrc     # zsh
echo 'source <(COMPLETE=bash przm)' >> ~/.bashrc   # bash
echo 'COMPLETE=fish przm | source' >> ~/.config/fish/config.fish
```

## AI assistants (MCP)

`przm mcp` is a [Model Context Protocol](https://modelcontextprotocol.io)
server: an AI assistant such as Claude Desktop or Claude Code can list your
sessions, open one, send commands, read the screen and run scripts. It only
sees the sessions you allow, on the **AI access** page of a session's (or a
folder's, or the global) settings:

| Access | The assistant can |
| --- | --- |
| none (default) | nothing: the session is not listed |
| read | open it and read the screen |
| full | also send commands and run scripts |

Even with full access, lines matching the **Blocked** list (regular
expressions, case ignored) are refused: by default restarts and shutdowns
(`reload`, `reboot`, `sudo shutdown`…), `write erase`, `erase`, `format`,
`delete`, `rm -r`, `mkfs` and `dd` onto a disk. New host keys are never
accepted from there: connect once in the app first.

Claude Desktop (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "przmterm": { "command": "/Users/you/.cargo/bin/przm", "args": ["mcp"] }
  }
}
```

Claude Code: `claude mcp add przmterm -- przm mcp`.

Sessions log in with their key or the password saved in the keychain. For
vault entries, add `"env": { "PRZMTERM_VAULT_PASSWORD": "…" }` to the server;
that file then holds the master password in plain text, so prefer keys or the
keychain for the sessions you share.

The tools: `list_sessions`, `open_session` (returns an id such as `s1`),
`send` (types a command, waits for the prompt and returns the output),
`read_screen`, `run_script` (a file of the scripts folder, or its source) and
`close_session`.

What a script run this way may do is in the
[scripting reference](SCRIPTING.md#scripts-through-mcp).

## Backup, sharing and sync

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

### Sessions from the PrzmTerm shell

The PrzmTerm shell (the first tab) saves sessions with one line, handy for
moving many hosts from another client:

```
add "Core DB" admin@10.0.0.5:2222 -f "Data Center" -d "Main database, call DBA"
add sw-core-01 10.0.0.1 -l netops -f "Data Center/Network" -J bastion
add rtr-lab telnet://10.0.0.9
add console serial:///dev/ttyUSB0@115200 -f Lab
```

The target is what Quick connect takes. `-f` is the folder (made if it is
not there), `-l` the user, `-p` the port, `-c` a vault credential, `-i` an
SSH key, `-J` a jump host and `-d` a description; quote what has spaces.
A session that exists is left alone unless `--replace` is added. `add` on
its own prints this help.

`-c` must name an entry of the password vault (letter case aside), and Tab
after it lists them. While the vault is locked only the entries other
sessions use can be checked: for another one PrzmTerm asks to unlock the
vault, then run the line again.

`export [text]` prints the `add` lines of the saved sessions (or those
whose name, address or description has the text). Copy them, paste them into
the PrzmTerm shell of another computer, and the sessions are there: where
they connect, how they log in, their folder and description; the rest
takes that PrzmTerm's defaults. Lines starting with `#` are comments.

`delete <session>` (or `rm`) deletes a saved session after asking `[y/N]`;
Tab completes the name. `delete <name> -f <folder>` deletes the session of
that name in that folder, and `delete -f <folder>` deletes a folder with the
sessions and folders in it, saying how many first (`delete <folder>` does
the same when no session has that name). Letter case does not matter for
folders, in `add` or `delete`, and Tab after `-f` completes the folder a
level at a time (`-f da` Tab gives `-f "Data Center/"`, Tab again lists or
completes what is in it).

### Several computers

**+** › *Settings folder…* moves the settings to any folder, such as one synced by iCloud
Drive, Dropbox or OneDrive, so several computers share the sessions. An empty
folder gets a copy of the current settings (the old ones stay where they
were); a folder that already holds PrzmTerm settings is used as it is, which is
how a second computer joins. Only `location.toml`, naming the folder, stays in
the default place. The `przm` CLI follows the same choice.

Computers on different 1.x versions can share a folder: a setting only a
newer version knows is left out by an older one, which says so (see
[Compatibility](CONFIG.md#compatibility)).

## Moving from SecureCRT

**+** next to *Sessions* › *Import from SecureCRT…* takes SecureCRT's XML
export (*File › Export Settings*) or session `.ini` files; *Import a SecureCRT
Sessions folder…* takes the whole `Config/Sessions` folder (macOS:
`~/Library/Application Support/VanDyke/SecureCRT/Config/Sessions`), or the
`Config` folder itself. A wizard then lists what was found, sessions by
folder, keyword sets and button groups, each with a box, and marks what is
already here; for those it skips them (the default: importing again adds
nothing), replaces them or keeps both (` (2)` after the name). Sessions land
in a `SecureCRT` folder with the same sub-folders, or the folder named in the
wizard. From the command line:

```sh
przm import-securecrt --dry-run export.xml     # show what would be imported
przm import-securecrt export.xml --into SecureCRT
```

Passwords are not imported: SecureCRT keeps them encrypted, and PrzmTerm asks
at the first login and can save them in the system keychain. Logon scripts,
port forwards and RDP or local-shell sessions are listed in the summary.

The button bars come along too: pick `ButtonBarV5.ini` from SecureCRT's
`Config` folder with the other files, or import the `Sessions` folder (the
bars next to it are read). Each bar becomes a button group (buttons already
there are left alone). Send String buttons keep their text and escapes. Send
Credentials buttons type the password of the [vault](#passwords-keychain-and-vault)
entry of the same name, so add that entry; like buttons whose label says
they type a password, they send only at a password prompt (*Only when*).
Buttons for menu functions, scripts or programs are listed as skipped.

## When something goes wrong

**+** › *Diagnostics…* (*PrzmTerm* › *Diagnostics…* on macOS) shows the
version, the system, the graphics card and driver in use, how the terminal
is drawn and where the log file is. *Copy details* copies it all, to paste
in an issue or a message.

PrzmTerm writes a log every time it runs: what it opened and closed,
imports, keyword sets not found, the graphics chosen, graphics errors and
crashes. It has no passwords. Each start keeps the run before as
`przmterm.previous.log` (*Open last run's log*), so a problem that went
away after a restart can still be looked at. The logs are in:

| System | Folder |
| --- | --- |
| Windows | `%LOCALAPPDATA%\PrzmTerm\logs` |
| macOS | `~/Library/Logs/PrzmTerm` |
| Linux | `~/.local/state/przmterm/logs` |

*Detailed log* adds PrzmTerm's steps in detail (and the graphics library's)
until it is turned off; turn it on, do what goes wrong, then send the log.
`RUST_LOG` (for example `RUST_LOG=debug`) sets the level by hand.

If the terminal's colours or cursor look wrong on one computer, the graphics
card is the first suspect. Start PrzmTerm with `PRZMTERM_RENDERER=egui` to
draw the terminal without the card's help, or with `WGPU_BACKEND=vulkan`
(Windows: Direct3D 12 by default) to try another way of using it. On
Windows, from a Command Prompt:

```
set PRZMTERM_RENDERER=egui
"%LOCALAPPDATA%\Programs\PrzmTerm\przmterm.exe"
```
