//! The script guide: how to write scripts, with snippets and the shipped
//! examples, and a small editor to check a script and save it to the
//! scripts folder.

use std::path::Path;

use eframe::egui;
use przmterm_script::{EXAMPLES, check};

/// A topic of the guide: what to read and a snippet to start from.
struct Topic {
    title: &'static str,
    text: &'static str,
    code: &'static str,
}

const TOPICS: [Topic; 7] = [
    Topic {
        title: "Getting started",
        text: "A script drives a session for you: it types commands, waits for the \
answers and reacts to them. Scripts are written in Rhai, a small language that looks \
like JavaScript or Rust.\n\n\
Run a script with Run Script… in the tab menu, put it on a button (the Script field \
of the button dialog), or set it as a session's Logon script so it runs each time the \
session connects. While it runs, the status bar shows its name and a Stop button.\n\n\
Edit the snippet below, press Check, give it a name and Save: it is then listed when \
you run a script.",
        code: r##"// Waits for the shell prompt and runs one command.
if session.wait_for("$ ", 10) {
    session.send("uptime\r");
    log("uptime sent to " + session.name);
}
"##,
    },
    Topic {
        title: "Sending text",
        text: "session.send(text) types text into the session, exactly as given. \
\"\\r\" is Enter, \"\\t\" is Tab and \"\\x03\" is Ctrl+C. Nothing is added: end a \
command with \"\\r\" to run it.",
        code: r##"session.send("terminal length 0\r");
session.send("show ip interface brief\r");
session.send("\x03");   // Ctrl+C
"##,
    },
    Topic {
        title: "Waiting for output",
        text: "session.wait_for(text, seconds) waits until text appears in the output \
and returns true, or false if the time runs out or the session closes. Without seconds \
it waits as long as it takes.\n\n\
With a list, session.wait_for([a, b, c], seconds) returns the position of the text \
that came first (0, 1, 2…) or -1.\n\n\
session.read_until(text, seconds) returns everything that came before text.\n\n\
Each wait uses up the output up to what it found, so the same prompt is not found \
twice. Colours and cursor moves are removed first. Tip: after sending a command, wait \
for its echo first, then for the prompt; older prompts are skipped that way.",
        code: r##"let command = "show version";
session.send(command + "\r");
session.wait_for(command, 10);              // the echo of the command
let output = session.read_until("#", 30);   // what it printed
log("got " + output.len() + " characters");

let seen = session.wait_for(["#", ">", "denied"], 10);
if seen == 2 { throw "access denied"; }
"##,
    },
    Topic {
        title: "Asking the user",
        text: "prompt(question) opens a box and returns what was typed; \
prompt_secret(question) hides the typing, for passwords. Stop script in the box stops \
the script. Answers are not saved anywhere.",
        code: r##"let host = prompt("Which host?");
let password = prompt_secret("Password for " + host);
session.send("ssh " + host + "\r");
if session.wait_for("assword:", 15) {
    session.send(password + "\r");
}
"##,
    },
    Topic {
        title: "Messages and pauses",
        text: "log(text) and print(text) show a line in the terminal, in colour; it is \
not sent to the session. sleep(ms) waits. throw \"message\" stops the script and shows \
the message in red.",
        code: r##"log("starting");
sleep(2000);   // two seconds
if !session.wait_for("ready", 5) {
    throw "the device is not ready";
}
"##,
    },
    Topic {
        title: "Session details",
        text: "session.name is the session's name, session.host its host (or serial \
port) and session.username the user name it logs in with: its vault entry's while the \
vault is open, otherwise the session's; empty when there is none.",
        code: r##"let user = session.username;
if user == "" { user = prompt("User name for " + session.name); }
log("logging in to " + session.host + " as " + user);
"##,
    },
    Topic {
        title: "Rhai in short",
        text: "Variables: let x = 1; text: \"a\" + \"b\" + 3. Conditions: if … { } \
else { }. Loops: for x in [1, 2, 3] { }, while … { }, loop { } with break. \
Functions: fn name(a, b) { … } — a function does not see the script's variables, so \
pass session to it. Text: .len(), .contains(x), .split(\";\"), .trim() (changes the \
text in place), .to_upper(). Every function is listed in docs/SCRIPTING.md in the \
project; the full language is described at rhai.rs.",
        code: r##"fn run(s, command) {
    s.send(command + "\r");
    s.wait_for(command, 10);
    s.wait_for("#", 30);
}

for command in ["show clock", "show users"] {
    run(session, command);
}
"##,
    },
];

enum Selected {
    Topic(usize),
    Example(usize),
}

pub struct ScriptGuide {
    selected: Selected,
    code: String,
    name: String,
    message: Option<(bool, String)>,
    /// A message for the status bar.
    pub notice: Option<String>,
}

impl ScriptGuide {
    pub fn new() -> Self {
        Self {
            selected: Selected::Topic(0),
            code: TOPICS[0].code.to_string(),
            name: String::new(),
            message: None,
            notice: None,
        }
    }

    fn select(&mut self, selected: Selected) {
        self.code = match &selected {
            Selected::Topic(i) => TOPICS[*i].code,
            Selected::Example(i) => EXAMPLES[*i].source,
        }
        .to_string();
        self.name = match &selected {
            Selected::Topic(_) => String::new(),
            Selected::Example(i) => EXAMPLES[*i].name.to_string(),
        };
        self.message = None;
        self.selected = selected;
    }

    /// Writes the editor's script to `<config>/scripts/<name>.rhai`.
    fn save(&mut self, config_dir: &Path) {
        let name = self.name.trim().trim_end_matches(".rhai").to_string();
        if let Err(e) = przmterm_session::validate_name(&name) {
            self.message = Some((false, e));
            return;
        }
        if let Err(e) = check(&self.code) {
            self.message = Some((false, format!("Not saved: {e}")));
            return;
        }
        let dir = config_dir.join(przmterm_session::SCRIPTS_DIR);
        let path = dir.join(format!("{name}.{}", przmterm_script::EXTENSION));
        if path.exists() {
            self.message = Some((
                false,
                format!("A script named {name} already exists; choose another name."),
            ));
            return;
        }
        match std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, &self.code)) {
            Ok(()) => {
                self.message = Some((true, format!("Saved {name}.")));
                self.notice = Some(format!(
                    "Saved the script {name}: run it from the tab menu or set it as a logon script."
                ));
            }
            Err(e) => self.message = Some((false, format!("Not saved: {e}"))),
        }
    }

    /// Draws the window; returns false once it is closed.
    pub fn show(&mut self, ctx: &egui::Context, config_dir: &Path) -> bool {
        let mut open = true;
        egui::Window::new(crate::theme::window_title(ctx, "Script guide"))
            .open(&mut open)
            .default_size([820.0, 560.0])
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    let mut pick = None;
                    ui.vertical(|ui| {
                        ui.set_width(190.0);
                        ui.strong("How to");
                        for (i, t) in TOPICS.iter().enumerate() {
                            let on = matches!(self.selected, Selected::Topic(n) if n == i);
                            if ui.selectable_label(on, t.title).clicked() {
                                pick = Some(Selected::Topic(i));
                            }
                        }
                        ui.add_space(8.0);
                        ui.strong("Examples");
                        for (i, e) in EXAMPLES.iter().enumerate() {
                            let on = matches!(self.selected, Selected::Example(n) if n == i);
                            if ui
                                .selectable_label(on, e.name)
                                .on_hover_text(e.about)
                                .clicked()
                            {
                                pick = Some(Selected::Example(i));
                            }
                        }
                    });
                    if let Some(p) = pick {
                        self.select(p);
                    }
                    ui.separator();
                    ui.vertical(|ui| {
                        let (title, text) = match self.selected {
                            Selected::Topic(i) => (TOPICS[i].title, TOPICS[i].text),
                            Selected::Example(i) => (EXAMPLES[i].name, EXAMPLES[i].about),
                        };
                        ui.heading(title);
                        egui::ScrollArea::vertical()
                            .id_salt("guide-text")
                            .max_height(150.0)
                            .show(ui, |ui| {
                                ui.label(text);
                            });
                        ui.add_space(6.0);
                        // The script takes the room left above the buttons.
                        let room = (ui.available_height() - 64.0).max(160.0);
                        egui::ScrollArea::vertical()
                            .id_salt("guide-code")
                            .max_height(room)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.code)
                                        .code_editor()
                                        .desired_rows(16)
                                        .desired_width(f32::INFINITY),
                                );
                            });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui
                                .button("Copy")
                                .on_hover_text("Copy the script to the clipboard")
                                .clicked()
                            {
                                ui.ctx().copy_text(self.code.clone());
                                self.message = Some((true, "Copied.".into()));
                            }
                            if ui
                                .button("Check")
                                .on_hover_text("Look for mistakes without running it")
                                .clicked()
                            {
                                self.message = Some(match check(&self.code) {
                                    Ok(()) => (true, "No mistakes found.".into()),
                                    Err(e) => (false, e),
                                });
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Save").clicked() {
                                        self.save(config_dir);
                                    }
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.name)
                                            .hint_text("script name")
                                            .desired_width(160.0),
                                    );
                                },
                            );
                        });
                        if let Some((ok, text)) = &self.message {
                            let color = if *ok {
                                egui::Color32::from_rgb(110, 200, 120)
                            } else {
                                ui.visuals().error_fg_color
                            };
                            ui.colored_label(color, text);
                        }
                    });
                });
            });
        open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_compile_and_saving_checks_names_and_clashes() {
        for t in &TOPICS {
            assert_eq!(check(t.code), Ok(()), "{}", t.title);
        }
        let dir = tempfile::tempdir().unwrap();
        let mut g = ScriptGuide::new();
        g.select(Selected::Example(0));
        assert_eq!(g.name, "linux-health");
        g.save(dir.path());
        assert!(dir.path().join("scripts/linux-health.rhai").is_file());
        g.save(dir.path());
        assert!(matches!(&g.message, Some((false, m)) if m.contains("already exists")));
        g.name = "bad/name".into();
        g.save(dir.path());
        assert!(matches!(&g.message, Some((false, _))));
        g.name = "broken".into();
        g.code = "let x = ;".into();
        g.save(dir.path());
        assert!(!dir.path().join("scripts/broken.rhai").exists());
    }
}
