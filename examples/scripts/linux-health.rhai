// Linux: a quick health check. Waits for the shell prompt, runs a few
// commands and logs a short summary in the terminal.
// Use it with "Run Script…" on a connected Linux session.

let prompts = ["$ ", "# "];

// Press Enter so a prompt shows up even if the login banner already passed.
session.send("\r");
if session.wait_for(prompts, 15) < 0 {
    throw "no shell prompt within 15 s";
}

session.send("uptime\r");
session.wait_for("uptime", 10);
let up = session.read_until("\n", 10);
up = session.read_until("\n", 10);
up.trim();
log("uptime: " + up);
session.wait_for(prompts, 10);

for command in ["df -h /", "free -m"] {
    session.send(command + "\r");
    session.wait_for(command, 10);   // the echo, then the prompt after the output
    session.wait_for(prompts, 10);
}

log("health check done on " + session.name);
