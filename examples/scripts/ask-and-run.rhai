// Any shell or network device: asks for commands (separated by ";") and
// runs them one after another, waiting for the prompt each time.

let line = prompt("Commands to run, separated by ;");
let prompt_text = prompt("What does the prompt end with? (e.g. $  #  >)");
prompt_text.trim();
if prompt_text == "" { prompt_text = "$"; }

let commands = line.split(";");
let count = 0;
for command in commands {
    command.trim();
    if command == "" { continue; }
    session.send(command + "\r");
    // First the echo of the command (this skips older prompts), then the
    // prompt that comes after its output.
    session.wait_for(command, 10);
    if !session.wait_for(prompt_text, 60) {
        log("no prompt after \"" + command + "\" within 60 s; stopping");
        break;
    }
    count += 1;
}
log("ran " + count + " command(s)");
