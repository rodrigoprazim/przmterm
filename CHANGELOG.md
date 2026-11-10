# Changelog

All notable changes to PrzmTerm are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/) as described in
[docs/VERSIONING.md](docs/VERSIONING.md).

## [Unreleased]

### Changed

- *New shortcut*: the text to send is a box of several lines, the fields
  reach the window's edge, and the choice of what the shortcut does is one
  segmented control.
- Settings files with keys this version does not know (written by a newer
  version, or misspelt) are read without those keys instead of being
  refused; the status line under the session list, the button bar,
  *Keyboard shortcuts…* or the terminal (for keyword sets) names them.
- `buttons.toml` calls the button groups `[[group]]` and
  `[[group.button]]`; files with `[[tab]]` are still read.
- A new look, in the style of today's dark editors: the Inter font for the
  interface, flat surfaces with thin borders and one blue accent, compact
  window title bars, buttons with icons that stay flat until the pointer is
  on them, and a closer session list.
- Session tabs look like editor tabs: flat on their strip, the active one in
  the terminal's colour and joined to it (no line between), the one under
  the pointer shaped as a tab too, × on the active tab and under the
  pointer.
- Settings and session windows: each page has a title, sections a larger
  title with a line under it, choices such as ask/always/never are one
  segmented control, combo boxes look like the text fields and line up with
  them, and Save/OK are blue. Check boxes and radio buttons are filled with
  the blue when on; the cursor's shape is a list of radio buttons and its
  blinking a check box. Window titles sit in the middle of their bar.
  The cursor rows have no "Default" or reset buttons, and Log to file is
  just yes/no; the log's file name field is as wide as the folder row.
- The button bar's groups are a drop-down list with **⋯** beside it to
  rename, add or delete them, so many groups no longer fill the bar.
- Menus (right-click and drop-down) have their entries closer together and
  are wider, so longer entries fit on one line.
- *Split Right* and *Split Down* are icons at the top right, with **⋯** for
  the grid and the arrangements, instead of the "◫ Split ▾" menu.

### Added

- Buttons and shortcuts can type passwords from the vault:
  `{vault:NAME}` in the text to send types the entry's password
  (`{vault:NAME:username}` its user name), added from the **Vault ▾** menu
  next to the text. Only the entry's name is kept in `buttons.toml` or
  `keymap.toml`; the vault is unlocked first when it is locked.
- The configuration reference, [docs/CONFIG.md](docs/CONFIG.md): every file
  and key, their defaults, and what stays compatible from 1.0 on.
- *Appearance…* (**+** menu, and the PrzmTerm menu on macOS): the interface
  in dark or light, or following the system's mode.
- The terminal's background and text colours can be chosen: in the
  settings (*Terminal* › *Colors*, for every session, a folder or one
  session, with a sample) and per tab in *Font, Colors and Cursor…*. The
  active tab wears the terminal's colours.
- Changing the global settings can now apply the new values to *all*
  sessions, including those with their own value: their own values for the
  changed fields are removed (the address is never touched). The other
  choices stay: sessions that use the default, or new sessions only.
- macOS: PrzmTerm's own menu bar. The app menu says PrzmTerm (not
  "przmterm") and has *About PrzmTerm*, *Settings…* (⌘,), *Keyboard
  Shortcuts…*, *Password Vault…*, *Keyword Sets…* and *Settings Folder…*;
  *Help* has the script guide and the project on GitHub. *Quit PrzmTerm*
  (⌘Q) asks first when sessions are open, as closing the window does.
- *About PrzmTerm* (also at the end of the **+** menu): the version, the
  developer and links to the project.
- *Check for Updates…* (PrzmTerm menu on macOS, **+** menu elsewhere): asks
  GitHub for the releases and points to the download page of a newer one.
  PrzmTerm also checks when it starts and shows a newer version in the
  status bar; a checkbox in the window turns that off (`preferences.toml`).
  Proxies set in `HTTPS_PROXY` and the system's certificates are used.
- The file browser (SFTP and SCP) sorts by a column when its header is
  clicked (again to reverse; folders stay on top), and has lines between
  the columns and under the headers.

### Fixed

- The session list indents sessions like folders: each name sits one step
  in from its folder's name, with a small session icon where folders have
  theirs.
- Recording a shortcut takes the whole combination again (Ctrl, Alt, Shift
  or ⌘ with a key): the modifier pressed first was recorded on its own.
- Anti-idle was not sent while the window was minimized or hidden: the
  sessions' work (reading, logging, triggers, scripts, transfers, anti-idle,
  reconnecting) now runs whether or not the window is drawn.
- Quick connects (the toolbar, and `ssh host` / `telnet host` in the
  PrzmTerm shell) ignored the global settings: they now take them (anti-idle,
  terminal, logging…), with what was typed (host, port, user) winning.
- A changed anti-idle reaches sessions already open, without reconnecting;
  and it no longer types into the password or keychain questions of an SSH
  login.

## [0.9.0] - 2026-11-08

### Added

- macOS releases come as `PrzmTerm.app` on a disk image (drag it to
  Applications) instead of loose binaries: it has an icon, sits in the Dock,
  opens without a Terminal window and asks for local network access like
  other apps. `przm` is inside the app. PrzmTerm has an icon on every
  system: the window, and the Windows executables.

- Release signing with free services: Windows executables signed through
  the SignPath Foundation (they now carry the product name and version),
  macOS binaries ad-hoc signed, and `SHA256SUMS.txt` signed with GPG. The
  README explains how to install on each system, how to check a download,
  and the code signing policy.

- The terminal grid has its own wgpu renderer: backgrounds, underlines and
  glyphs are drawn as one batch per pane, glyphs rasterised once into an
  atlas, instead of laying the text out every frame. Colour emoji and
  characters only a system font has are still drawn by egui.
  `PRZMTERM_RENDERER=egui` brings back egui's painting.

- Tab groups: *Group* in the tab menu starts a group or adds a tab to one;
  the group's tabs sit together after a coloured label, each starting with a
  bar of the group's colour; new groups take the lowest free "Group N". The
  label offers *Send
  Command to Group…*, *Rename Group…*, *Ungroup* and *Close Group*.
- Several windows: *Window › Move to New Window* (with its live connection),
  *Clone to New Window* and *Move to Main Window* in the tab menu. Each
  window has its own tabs; next/previous tab stays within it.
- *Lock…* in the tab menu locks a tab with a password: its screen is hidden,
  and it takes no typing, does not close and cannot be cloned or browsed
  until unlocked. The password is kept in memory only, hashed.
- `przm mcp`: a Model Context Protocol server, so AI assistants (Claude
  Desktop, Claude Code…) can list sessions, open them, send commands, read
  the screen and run scripts. Each session (or folder, or the defaults) sets
  its **AI access** on a page of its own: none (the default: not even
  listed), read or full; a block list of regular expressions (by default
  restarts, erasing, formatting, `rm -r`, `mkfs`, `dd` onto disks) is never
  sent, also from scripts. New host keys are never accepted there.
- `przm run <session> -c …`: connects a session without a window, logs in
  (key, vault entry or the password saved in the keychain; other questions
  asked on the terminal), runs the commands one after another, waiting for
  the prompt after each, and prints the output. Commands can also come from
  stdin. Exit code 0, 1 (connection or login failed) or 2 (a prompt did not
  come back in time).
- CI checks the dependencies with `cargo deny` on every push and weekly:
  RustSec security advisories, licences and crate sources (`deny.toml`).
- A scroll bar beside the terminal: it shows where the view is in the
  history; drag it or click its track to move.
- Pasting text with line breaks first shows it in a box, where it can be
  checked or edited: *Paste* (or Ctrl/⌘+Enter) sends it, *Cancel* drops it.
  The box is wide and can be resized from its corner.
- Panes of a split tab move: drag one by its caption onto another to swap
  them (the target lights up), or right-click the caption for *Move
  Left/Right* (*Up/Down* when stacked).
- Font and cursor per session: *Font and Cursor…* in the tab menu (was
  *Font…*) and the session settings pick the font (built-in or any installed
  monospace font), its size, the cursor shape (block, underline, bar), its
  colour and blinking. Changes show at once; *Cancel* (or Esc) undoes them,
  *Apply* keeps them in the tab and *Save* also in the session's settings.
- Anti-idle: a session can send one character (a space, say), as it is
  and without Enter, after a number of seconds without typing, so
  devices that drop idle sessions keep it open
  (`anti_idle`, `anti_idle_interval`; *Automation* page of the editor).
  SecureCRT's "Send string every N seconds" is imported.
- *Settings folder…* (under **+**) moves PrzmTerm's settings to another
  folder, a synced one for instance: an empty folder gets a copy of the
  current settings, a folder that already has PrzmTerm settings is used as it
  is. The `przm` CLI follows the choice.

### Changed

- The tab PrzmTerm opens with (the demo shell) is now a PrzmTerm shell that
  connects: `ssh <session>` opens a saved session, with Tab completing the
  name (or the folder path), `ssh [-p port] [-l user] user@host` and `telnet
  host [port]` connect to hosts that are not saved, `sessions [text]` lists
  them, and Up/Down recall earlier lines. Another one opens with **+** at
  the end of the tabs, a double click on the free part of the tab bar or
  Ctrl+Shift+N (⌘⇧N on macOS), or next to a session with *Split* (*A new
  PrzmTerm shell*).

- The session, folder and global settings are split into pages chosen from
  a menu on the left (Connection, Login, SSH, Serial port, Terminal,
  Automation, Logging): the window grows with the page instead of showing a
  scroll bar (one appears only when the screen is too short). Related
  options sit under a title (Tunnels, Anti-idle, Triggers, Font, Cursor)
  instead of repeating it in every label; the explanations open with **?**
  beside the title, like the help for log file names under its field.
  Tunnels and triggers are added with **+** (beside the first one's ×);
  removing the last one goes back to those from above.
- Fonts: the list shows every installed font that can draw text, the
  monospace ones first and the rest apart, with a search box; fonts that
  cannot be drawn (bitmap-only, emoji) are left out.
- One button size and drawn icons in every dialog: reset (↺), remove (✕),
  the lists of shells and jump hosts, and the SFTP browser's parent folder
  and refresh.
- The locked password vault and the unlock asked for from the session
  editor open as small windows, as wide as the password field.
- A session tab holds its close button: the title, a thin divider and a ✕
  in the tab's own colours, in one box; the whole close part is the button
  and lights up on its own.
- With *Send to all* in a split tab, every pane that gets the keys has the
  blue frame of the active pane (instead of an amber one); the status bar
  button is blue too.
- The *Log* button has the size and shape of the other status bar buttons
  (still red when not logging, green when logging), with a drawn dot; the
  button bar's *+* matches the other icon buttons.
- The session list's buttons (hide, reload, lock, settings, +) are drawn
  icons of one size, and the symbols in other buttons (◫ ▾ ⇱ ⇅ ▤ ●) sit on
  the text's line.

### Fixed

- `ssh` and `open` in the PrzmTerm shell take session names with spaces and
  dashes (`ssh Lab/Core - DB`, quotes allowed), and Tab completes them.
- Tab in the PrzmTerm shell lists many matching sessions under their
  folders, in aligned columns that fit the window, with a wider gap when
  names have spaces.
- The PrzmTerm shell tab uses the font and cursor of the global settings
  (and follows them when they change).
- Session list: with the padlock open, folders can be dragged too (into
  another folder or to the top level), and the grip and folder icons are
  drawn, so they line up with the names.
- The session list's horizontal scroll bar no longer covers the last
  session: the scroll bars take their own room.
- Quotes and accents typed with dead keys (ABNT2 and international layouts)
  reach the terminal; the key waiting for the next one shows at the cursor.
- Text re-wraps when the terminal's width changes, so lines cut by a split
  come back whole when it is separated (or the window widens).
- A session tab's tooltip names where it is connected (`SSH user@host:port`,
  or the serial line); a split tab lists each session.
- Button bar: the tooltip shows the description, or else what the button
  sends or the script it runs (none when it has neither). *Show tooltip* is
  set per button, in its menu and in the button editor; a tab's menu shows
  or hides them all for that tab. (0.8.0's switch for the whole bar is read
  as off on every button.)
- Session editor: with the vault locked, the vault entry list offers the
  entries the sessions use (and the one set), plus *Unlock to see every
  entry*.

## [0.8.0] - 2026-11-03

### Added

- SFTP browser: *File Browser › SFTP* in the tab menu or in a session's menu
  in the list (connects first if needed), or a keyboard shortcut, opens a
  window on the same SSH connection, with no new login. Browse folders (path
  field, parent, refresh, hidden files), upload files or folders (also by
  dropping them on the window), download files or folders (double-click a
  file, or *Download…*), with progress and *Cancel*; new folder, rename and
  delete (folders with their contents, after asking). Downloads never
  overwrite: an existing name gets ` (2)`.
- File transfers in the terminal with Xmodem, Xmodem-1K, Ymodem and Zmodem
  (**⇅ Transfer** in the status bar, with *Send* and *Receive* tabs and the
  protocol in a list, Zmodem first; or shortcuts), for
  serial consoles and Telnet as well as SSH. Running `sz` on the other side
  downloads to the Downloads folder by itself; running `rz` asks for the
  files to send. On a Unix host the dialog can type the `rz`/`sz`/`rb`/`sb`/
  `rx`/`sx` command for you. The status bar shows the file and progress with
  *Cancel* (Ctrl+C works too); received files never overwrite others, and a
  cancelled one is removed. When the typed command does not exist on the
  other side ("command not found", "Unknown command"…), nothing is sent to
  the shell and a note says to install lrzsz or use SFTP.
- SCP on SSH sessions, as in WinSCP. *File Browser › SCP* in the tab menu or
  in a session's menu in the list (connects first if needed) opens the file
  browser (the same as for SFTP) over SCP, its item showing the shell: folders listed with
  `ls`, new folder, rename and delete with `mkdir`, `mv` and `rm`, uploads
  and downloads with `scp`. The *File transfer* dialog has *SCP* too, beside
  the terminal, with progress and *Cancel* in the status bar.
- *SCP shell* in the session settings: *Default* (the login shell),
  `/bin/bash`, `/bin/ksh`, `sudo su -`, `su -` or any other. With `sudo su -`
  or `su -` everything runs as root; PrzmTerm answers the password prompt
  (asked once per window) and reports a wrong password. Received files never
  overwrite others.

### Changed

- *File Browser* in the tab menu and in a session's menu holds *SFTP* and
  *SCP* (with the session's SCP shell) in one submenu.
- Dialog buttons sit on the right, the one that goes ahead (*Save*, *OK*,
  *Delete*, *Split*…) last and the one that backs out (*Cancel*, *Back*,
  *Close*) just before it.
- The *File transfer* dialog's *Send* and *Receive* tabs share its whole
  width.
- While *Send to all* is on, the status bar button reads *⇶ Typing to all*
  in an outline, and every pane that gets the keys is framed with a *typing
  to all* tag in its caption.
- The status bar *Log* button names the log file while logging (*● Log*
  and the file's name, still green; *○ Log* in red when not). Click still
  starts or stops logging. Right-click opens a menu with the file's name,
  *Start/Stop Logging*, *Open Log* (in the default text editor), *Show in
  Finder* (Explorer, or the folder on Linux) and *Log Settings…*, which opens
  the session editor at its log options.
- Symbols in the interface (⇱, ⇶, ⇅, ◫, ▤…) no longer show as empty squares
  on systems whose fonts lack them.
- In a split tab, **⇱ Separate** at the top right gives each pane its tab
  back; it replaces *× Pane* (closing a pane stays on Ctrl+Shift+W / ⌘⇧W).
- The SFTP window opens in the middle of the window; dragging over the list
  selects several entries (Ctrl/⌘ adds), and Shift+click selects a range.

## [0.7.0] - 2026-11-02

### Added

- Scripts in [Rhai](https://rhai.rs) (`<config>/scripts/*.rhai`): a
  `session` object with `send`, `wait_for` (one text or a list, with a
  timeout), `read_until`, `name`, `host` and `username`, plus `log`/`print`,
  `prompt`, `prompt_secret` and `sleep`. Run them with *Run Script…* in the
  tab menu, from a button (*Script* in the button dialog) or as a session's
  *Logon script*, run each time it connects. The status bar shows the running
  script with **■ Stop**. Scripts are included in backups.
- Keyboard shortcuts (**+** › *Keyboard shortcuts…*): change or delete the
  built-in ones and add new ones that send text (with the button escapes),
  run an app command (19 of them: tabs, split, reconnect, log, scripts…),
  run a script or press a button. Keys are recorded by pressing them;
  clashes are refused and keys that would get in the way of typing are
  flagged. Saved in `<config>/keymap.toml` (and in backups); *Reset to
  defaults* goes back to the built-in ones. Menus and tooltips show the keys
  in use.
- Script guide (**+** › *Script guide…*, or *Script Guide…* in the tab menu):
  how to write scripts, topic by topic, with a snippet for each and six
  ready examples (Linux health check, run a list of commands, watch a host
  with ping, Cisco and Huawei show commands, a generic logon script). The
  snippet can be edited, checked for mistakes and saved as a new script.
  The examples are also in `examples/scripts/`.

## [0.6.0] - 2026-11-02

### Added

- Keyword highlighting: named keyword sets in `<config>/keywords/*.toml`,
  each a list of whole words or regular expressions (lookaround allowed)
  with a text colour, optional background and bold. Chosen per session,
  folder or globally (*Keywords* in the editor, `keywords = "NAME"`). Where
  matches overlap the longest wins; only text the remote side left in the
  default colour is recoloured.
- Button bar with macro buttons in tabs (*▤ Buttons* in the status bar,
  `<config>/buttons.toml`): a button sends text with `\r` (Enter), `\t`,
  `\e`, `\n` and `\p` (one-second pause) to the current session. Each has
  an optional description, shown as its tooltip; tooltips can be turned off
  (right-click the bar). The button dialog has *Clear*. Right-click a button
  to edit, move or delete it, and a tab to rename, add or delete tabs.
- *⇶ Send to all* in the status bar of a split tab: after a warning, what is
  typed goes to every pane of that tab until turned off (or the tab changes).
- Search in the session list: filters by name, host or IP; Enter connects
  to the first match, Esc clears it.
- Triggers (*Triggers* in the session editor, `[[trigger]]` in session files):
  when text or a regular expression shows up in the output, PrzmTerm types
  the answer, with the button escapes (`\r`, `\p`…). Matches ignore colours
  and cursor moves, work across reads, never answer the same text twice, and
  can be limited to once per connection. Inherited from folders and globals
  like tunnels.
- Quitting with sessions connected asks first; so does closing a tab or pane
  whose session is connected.
- Tab menu (right-click a tab): *Rename…* (only the tab, until it closes),
  *Reset Name*, *Reconnect*, *Disconnect*, *Close*, *Close Disconnected
  Tabs*, *Close Other Tabs*, *Close Tabs to the Right*, *Close All Tabs*,
  *Clone Session*, *Session Options…* (or *Save Session…* for a quick-connect
  session), *Font…* (size, for the tab) and *Send Commands to This Tab* (split
  tabs).
- Keyword set editor (**+** › *Keyword sets…*, or *Edit sets…* next to
  *Keywords* in the session editor): create, rename and delete sets; add,
  find, reorder and remove rules with colour pickers; a live preview on
  editable sample text; *Cancel* drops unsaved changes, and closing or
  switching sets with unsaved changes asks first. Open panes using a changed
  set update at once.
- SecureCRT keyword lists (`Config/Keywords/*.ini`) import with the other
  SecureCRT files, and imported sessions keep their keyword set.
- Settings backup, export and import, in `.przm` files (plain TOML text).
  **+** › *Back up settings…* saves everything (global settings, folders,
  sessions, keyword sets, buttons, known hosts; not logs) and *Restore or
  import…* puts a backup back, on this or another machine, after saving the
  current settings in `<config>/backups/`. *Export folder…* and *Export
  session…* (right-click in the session list) save a part to share with
  someone; importing it adds a new folder and never replaces anything.
  Passwords are never included: they stay in the system keychain.
- Password vault (**+** › *Password vault…*): user names, passwords and key
  passphrases in one encrypted file, `<config>/vault.toml` (Argon2id key
  from a master password, XChaCha20-Poly1305; user names and entry names are
  encrypted too). A session, folder or the global settings choose an entry
  under *Vault entry*, so one change updates every device using it. Opening
  such a session asks for the master password once; *Lock* forgets the
  entries again. Renaming an entry updates the sessions that use it. A stale
  vault password falls back to asking in the terminal. Backups carry the
  vault still encrypted; exports never do. The OS keychain stays available
  for sessions without an entry.

### Changed

- *Split* is one menu (**◫ Split ▾**): *Grid (two per row)* (new, first,
  Ctrl+Shift+G / ⌘⇧G), *Side by side* and *Stacked* each ask which sessions
  open in other tabs join the current one (several at once); they move into
  this tab, and a tab left empty closes. To split a session with itself,
  clone it first (*Clone Session*). In a split tab the same menu changes the
  arrangement, moves the active pane to a tab of its own or separates all
  panes into tabs (also *Separate Panes into Tabs* in the tab menu), so
  nothing has to be closed.
- Each pane of a split tab has a caption with its session's name, the tab's
  tooltip lists them, and the session list marks connected sessions with a
  green dot.
- The password vault window: entry rows use the whole width; *Lock* is on
  the left, *Change master password…* and *New entry…* on the right.
- Messages no longer say where files are kept (log files, backups, the
  vault, session files); error messages still name a file that fails.

## [0.5.0] - 2026-11-02

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
- X11 forwarding (`x11_forwarding`, *X11 forwarding* in the editor), like
  `ssh -X`: remote graphical programs open on the local X server (XQuartz on
  macOS, VcXsrv and the like on Windows, the desktop on Linux). The server
  gets a random cookie; PrzmTerm swaps in the display's real one (from
  `xauth`) for each connection and refuses connections without it. Imported
  SecureCRT sessions keep their *Forward X11* setting.
- Legacy SSH algorithms per session (`legacy_algorithms`, *Legacy algorithms*
  in the editor) for old switches and routers: SHA-1 key exchange
  (`diffie-hellman-group1/14/exchange-sha1`), CBC and 3DES ciphers,
  HMAC-SHA1 and DSA host keys, offered after the modern ones. A connection
  that uses old algorithms shows a **legacy** badge in the status bar and ⚠ on
  its tab. When a server offers only old algorithms and the option is off,
  the error says so and which ones it offers.
- NIST ECDH key exchanges (`ecdh-sha2-nistp256/384/521`) are offered by default.
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
- The **+** after the tabs is gone (*Clone Session* in the tab menu, or
  Ctrl+Shift+T / ⌘⇧T); *× Pane* shows only in a split tab; the button
  dialog has *Clear* on the left and *Save* / *Cancel* on the right.
- The session list reaches up to the toolbar; the tabs of open sessions and
  the button bar span the terminal only.
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

[Unreleased]: https://github.com/rodrigoprazim/przmterm/compare/v0.9.0...main
[0.1.0]: https://github.com/rodrigoprazim/przmterm/releases/tag/v0.1.0
[0.2.0]: https://github.com/rodrigoprazim/przmterm/compare/v0.1.0...v0.2.0
[0.3.0]: https://github.com/rodrigoprazim/przmterm/compare/v0.2.0...v0.3.0
[0.4.0]: https://github.com/rodrigoprazim/przmterm/compare/v0.3.0...v0.4.0
[0.5.0]: https://github.com/rodrigoprazim/przmterm/compare/v0.4.0...v0.5.0
[0.6.0]: https://github.com/rodrigoprazim/przmterm/compare/v0.5.0...v0.6.0
[0.7.0]: https://github.com/rodrigoprazim/przmterm/compare/v0.6.0...v0.7.0
[0.8.0]: https://github.com/rodrigoprazim/przmterm/compare/v0.7.0...v0.8.0
[0.9.0]: https://github.com/rodrigoprazim/przmterm/compare/v0.8.0...v0.9.0
