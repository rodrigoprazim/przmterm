// Cisco IOS / IOS-XE / NX-OS: turns paging off and runs a few "show"
// commands, then logs how long the running configuration is.

session.send("\r");
let seen = session.wait_for(["#", ">"], 15);
if seen < 0 { throw "no device prompt within 15 s"; }
if seen == 1 {
    // User EXEC (">"): ask for the enable password.
    session.send("enable\r");
    if session.wait_for("Password:", 5) {
        session.send(prompt_secret("Enable password for " + session.name) + "\r");
    }
    if !session.wait_for("#", 10) { throw "enable failed"; }
}

session.send("terminal length 0\r");
session.wait_for("#", 10);

for command in ["show clock", "show version | include uptime", "show ip interface brief"] {
    session.send(command + "\r");
    session.wait_for(command, 10);   // the echo, then the prompt after the output
    session.wait_for("#", 30);
}

session.send("show running-config\r");
session.wait_for("show running-config", 10);
let config = session.read_until("\nend", 120);
let lines = config.split("\n").len();
log("running-config: " + lines + " lines");
session.wait_for("#", 10);
