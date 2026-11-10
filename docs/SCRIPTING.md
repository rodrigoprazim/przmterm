# Scripting reference

PrzmTerm scripts drive a session for you. A script types commands, waits
for the answers, reads them and reacts. Scripts are written in
[Rhai](https://rhai.rs), a small language that looks like JavaScript or
Rust. This page lists everything a script can use. *Script guide…* (in the
**+** menu, or *Help* on macOS) teaches the same with snippets you can edit,
check and save.

- [Running a script](#running-a-script)
- [A first script](#a-first-script)
- [How waiting works](#how-waiting-works)
- [The `session` object](#the-session-object)
- [Other functions](#other-functions)
- [Errors and stopping](#errors-and-stopping)
- [Scripts through MCP](#scripts-through-mcp)
- [Rhai in short](#rhai-in-short)
- [Examples](#examples)

## Running a script

Scripts are text files ending in `.rhai`, normally kept in the `scripts`
folder of the [settings folder](CONFIG.md#where-the-settings-live). Where a
script is named, a bare name (`cisco-show.rhai`) is looked for there, and a
full path (or `~/…`) is used as it is.

| From | How |
| --- | --- |
| A tab | *Run Script…* in the tab's menu picks a file. |
| A button | The *Script* field of the button dialog (`script` in [`buttons.toml`](CONFIG.md#button-bar-buttonstoml)). |
| A shortcut | *Run script* in the shortcut dialog (`script` in [`keymap.toml`](CONFIG.md#keyboard-shortcuts-keymaptoml)). |
| Each connection | The session's **Logon script** (`logon_script`). It starts with the connection, and again on every reconnect. |
| An AI assistant | The MCP tool `run_script`, with a file name or the source itself ([below](#scripts-through-mcp)). |

A script runs in the pane where it was started, one at a time: starting
another stops the one that was running. While it runs, the status bar
shows its name and **■ Stop**. The terminal shows where it starts, and
also what it logs (in cyan) and why it failed (in red). None of these
lines reach the session.

## A first script

```rhai
// Log in to a switch that asks for its own user name and password.
if session.wait_for("Username:", 15) {
    session.send(session.username + "\r");
    session.wait_for("Password:");
    session.send(prompt_secret("Password for " + session.name) + "\r");
}

let seen = session.wait_for(["#", ">", "denied"], 10);   // index, or -1
if seen == 2 { throw "access denied"; }
if seen == 0 {
    session.send("terminal length 0\r");
    session.wait_for("#");
    session.send("show version\r");
    let version = session.read_until("#", 10);   // the output before "#"
    log("version: " + version.len() + " characters");
}
```

## How waiting works

A script sees the session's output from the moment it starts, as plain
text: colours, cursor moves and other escape sequences are removed first.

- **Every wait uses up the output up to what it found.** The next wait
  starts after it, so the same prompt is never found twice. Output that
  came *before* the match is used up too.
- **Output waits for the script.** Text that arrives while the script does
  something else (`sleep`, a `prompt`) is kept for the next wait. The script
  keeps the latest 32 to 64 KiB of output, so a very long burst loses its
  oldest part.
- **Wait for the echo first.** After `session.send("show run\r")`, the
  output still holds the prompt the command was typed at. Wait for the
  command's echo first (`session.wait_for("show run")`), then for the next
  prompt.
- **Matches are exact.** A wait looks for the text as written: no regular
  expressions, and case counts.
- **Time limits are in seconds**, whole (`10`) or not (`2.5`). No limit, or
  `0` and below, waits as long as it takes, until the session closes.

## The `session` object

`session` is the session the script runs in.

### Properties

| Property | Is |
| --- | --- |
| `session.name` | The session's name, or the tab's title for a tab that has no saved session. |
| `session.host` | Its host, or the serial port. Empty for a local shell. |
| `session.username` | The user name it logs in with: from its vault entry, if it has one and the vault is open, otherwise from the session. Empty when there is none. |

### `session.send(text)`

Types `text` into the session, exactly as given; nothing is added. End a
command with `"\r"` (Enter) to run it. Rhai strings know `"\r"`, `"\n"`,
`"\t"`, `"\x03"` (Ctrl+C), `"\x1b"` (Escape) and `"\u{…}"`.

```rhai
session.send("terminal length 0\r");
session.send("\x03");   // Ctrl+C
```

Unlike buttons, scripts take no `\p` pause or `{vault:…}` piece in `send`:
use `sleep` for a pause, and `prompt_secret` or the logon itself for
secrets.

### `session.wait_for(text)`, `session.wait_for(text, seconds)`

Waits until `text` appears in the output. Returns `true` when it does, and
`false` if the session closes first or the time runs out.

```rhai
if !session.wait_for("$ ", 10) {
    throw "no shell prompt";
}
```

### `session.wait_for([a, b, …])`, `session.wait_for([a, b, …], seconds)`

Waits for whichever of the texts comes first. Returns its position in the
list (`0`, `1`, `2`…), or `-1` if the session closes or the time runs out.
When two texts are found at the same place, the earlier one in the list
wins.

```rhai
let seen = session.wait_for(["#", ">", "assword:"], 15);
if seen == 2 { session.send(prompt_secret("Enable password") + "\r"); }
```

### `session.read_until(text)`, `session.read_until(text, seconds)`

Waits for `text` like `wait_for`, and returns the output that came before
it (`text` itself is used up but not included). Instead of returning
`false`, it is an error when the session closes or the time runs out: the
script stops there, unless the error is caught.

```rhai
let command = "show clock";
session.send(command + "\r");
session.wait_for(command, 10);              // the echo
let clock = session.read_until("#", 10);    // what the command printed
log(clock);
```

## Other functions

| Function | Does |
| --- | --- |
| `log(text)` | Shows `[script] text` on a line of its own in the terminal, in cyan. Nothing is sent. |
| `print(text)` | The same as `log`. |
| `debug(value)` | Shows the value with where in the script it was called. |
| `sleep(ms)` | Waits `ms` milliseconds. *Stop* still works while it waits. |
| `prompt(question)` | Opens a box with `question` and returns what was typed. |
| `prompt_secret(question)` | The same, with the typing hidden, for passwords. |

Answers to `prompt` and `prompt_secret` are not saved anywhere. *Stop
script* in the box stops the script.

## Errors and stopping

- `throw "message"` stops the script with that message. The terminal shows
  it in red, and the status bar says the script failed.
- A mistake in the script (a typo, a wrong type, calling something that
  does not exist) stops it the same way, with Rhai's message and where it
  happened. *Check* in the script guide finds the typing mistakes without
  running the script.
- `try { … } catch (e) { … }` catches errors, `read_until`'s time-outs
  among them:

  ```rhai
  try {
      let out = session.read_until("#", 5);
      log(out);
  } catch (e) {
      log("no prompt: " + e);
  }
  ```

- **■ Stop**, *Stop script* in a prompt, starting another script in the
  same pane, or closing the pane stops a running script at once, even in
  the middle of a wait or a `sleep`. Stopping is not an error.
- Scripts cannot read or write files or start programs on your computer:
  they only talk to the session.

## Scripts through MCP

An AI assistant connected with `przm mcp` can run a script on a session it
opened, when that session's AI access is *full*. It gives either the
`script` file name (in the scripts folder) or the `source` itself, and an
optional `timeout` in seconds (120 by default; at most 3600).

- The session's **Blocked** list applies to everything the script sends.
  A line that matches stops the script, and nothing of that line is sent.
- `prompt` and `prompt_secret` have nobody to ask there: they stop the
  script.
- The assistant gets back how the script ended, what it logged and the
  session's output while it ran.

## Rhai in short

| | |
| --- | --- |
| Variables | `let x = 1;` `const LIMIT = 5;` |
| Text | `"a" + "b" + 3` makes `"ab3"`; `` `x is ${x}` `` fills in values |
| Conditions | `if x > 1 { … } else if … { … } else { … }`; `switch x { 1 => …, _ => … }` |
| Loops | `for x in [1, 2, 3] { … }`, `for i in 0..10 { … }`, `while … { … }`, `loop { … }` with `break` and `continue` |
| Lists | `let l = ["a", "b"]; l.push("c"); l.len(); l[0]` |
| Maps | `let m = #{ host: "a", port: 22 }; m.host; m["port"]` |
| Functions | `fn run(s, command) { … }` |
| Text methods | `.len()`, `.contains(x)`, `.starts_with(x)`, `.ends_with(x)`, `.index_of(x)`, `.sub_string(from, len)`, `.split(";")`, `.split_rev(x)`, `.replace(a, b)`, `.to_upper()`, `.to_lower()`, `.trim()`, `.parse_int()` |

Two things that surprise people:

- `.trim()` and `.replace(a, b)` **change the text in place** and return
  nothing. Write `name.trim();`, not `name = name.trim();`.
  (`.to_upper()` and `.to_lower()` do return a new text.)
- A function **does not see the script's variables**, `session` included.
  Pass what it needs:

  ```rhai
  fn run(s, command) {
      s.send(command + "\r");
      s.wait_for(command, 10);
      s.wait_for("#", 30);
  }
  for command in ["show clock", "show users"] {
      run(session, command);
  }
  ```

The whole language is described in [the Rhai book](https://rhai.rs/book/).

## Examples

These come with PrzmTerm, in [`examples/scripts`](../examples/scripts) and in
the script guide, which can save a copy to your scripts folder:

| Script | Does |
| --- | --- |
| `linux-health` | Linux: uptime, disk and memory, with a summary line. |
| `ask-and-run` | Any device: asks for commands separated by `;` and runs them, waiting for the prompt each time. |
| `ping-watch` | Linux and macOS: logs when a host goes up or down, until stopped. |
| `cisco-show` | Cisco: enable if needed, paging off, a few `show` commands. |
| `huawei-show` | Huawei VRP: paging off and `display` commands. |
| `logon-generic` | A logon script that answers the user name and password prompts. |
