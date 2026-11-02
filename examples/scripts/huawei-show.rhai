// Huawei VRP: paging off for this session, then a few "display" commands.
// The prompt looks like <SWITCH> or [SWITCH].

session.send("\r");
if session.wait_for([">", "]"], 15) < 0 { throw "no device prompt within 15 s"; }

session.send("screen-length 0 temporary\r");
session.wait_for([">", "]"], 10);

for command in ["display clock", "display version", "display interface brief"] {
    session.send(command + "\r");
    session.wait_for(command, 10);   // the echo, then the prompt after the output
    if session.wait_for([">", "]"], 60) < 0 {
        log("no prompt after " + command);
        break;
    }
}
log("done on " + session.name);
