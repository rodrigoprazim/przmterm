// Logon script for devices that ask for the user name and password inside
// the terminal (Telnet, serial consoles, some SSH servers).
// Set it under "Logon script" in the session; it runs at every connection.
// The user name comes from the session (or its vault entry); the password
// is asked once per connection.

let seen = session.wait_for(["sername:", "ogin:", "assword:"], 30);
if seen < 0 {
    log("no login prompt in 30 s; nothing to do");
    return;
}
if seen < 2 {
    let user = session.username;
    if user == "" { user = prompt("User name for " + session.name); }
    session.send(user + "\r");
    if !session.wait_for("assword:", 15) { throw "no password prompt"; }
}
session.send(prompt_secret("Password for " + session.name) + "\r");

let result = session.wait_for(["#", ">", "$ ", "ncorrect", "failed", "denied"], 20);
if result >= 3 { throw "login refused"; }
if result < 0 { throw "no prompt after the password"; }
log("logged in to " + session.name);
