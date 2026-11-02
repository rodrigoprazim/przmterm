// Linux / macOS: pings a host every few seconds and logs when it goes up or
// down. Runs until you press "Stop" in the status bar.

let target = prompt("Host to watch (name or IP)");
target.trim();
if target == "" { throw "no host given"; }
let prompts = ["$ ", "# "];
let last = "";

loop {
    // The quotes make the echoed command line differ from the answer, so
    // only the real "PING-UP" / "PING-DOWN" is matched.
    session.send("ping -c 1 -W 2 " + target + " >/dev/null 2>&1 && echo 'PING''-UP' || echo 'PING''-DOWN'\r");
    let state = if session.wait_for(["PING-UP", "PING-DOWN"], 10) == 0 { "up" } else { "down" };
    session.wait_for(prompts, 10);
    if state != last {
        log(target + " is " + state);
        last = state;
    }
    sleep(5000);
}
