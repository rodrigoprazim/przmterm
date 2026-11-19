//! `przm add`, `delete`, `export` and `import`: the PrzmTerm shell's
//! commands for saved sessions, from a system shell (and scripts), with the
//! same rules ([`przmterm_session::commands`]). Also the Tab completion of
//! folders, sessions and vault entries for bash, zsh and fish.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use przmterm_session::commands::{
    AddSpec, VaultMiss, add_lines, add_session, count_inside, export_line, found_folder,
    split_args, subfolders, vault_entry,
};
use przmterm_session::{SessionTree, vault};

use crate::headless::{Asker, TerminalAsker};

#[derive(clap::Args)]
pub struct AddArgs {
    /// The session's name.
    pub name: String,
    /// Where it connects: [user@]host[:port], telnet://host[:port] or
    /// serial://port[@baud].
    pub target: String,
    /// The folder, e.g. "Data Center/Network" (made if it is not there).
    #[arg(short = 'f', long, value_name = "FOLDER", add = ArgValueCompleter::new(complete_folder))]
    pub folder: Option<String>,
    /// The user (or user@ in the target).
    #[arg(short = 'l', long, value_name = "USER")]
    pub user: Option<String>,
    /// The port (or :port in the target).
    #[arg(short = 'p', long, value_name = "PORT")]
    pub port: Option<String>,
    /// An entry of the password vault.
    #[arg(short = 'c', long, value_name = "ENTRY", add = ArgValueCompleter::new(complete_credential))]
    pub credential: Option<String>,
    /// An SSH private key.
    #[arg(short = 'i', long = "key", value_name = "FILE")]
    pub key: Option<String>,
    /// Connect through this host (a saved session or user@host).
    #[arg(short = 'J', long = "jump", value_name = "HOST", add = ArgValueCompleter::new(complete_session))]
    pub jump: Option<String>,
    /// A description.
    #[arg(short = 'd', long, value_name = "TEXT")]
    pub description: Option<String>,
    /// Write over a session of that name.
    #[arg(long)]
    pub replace: bool,
}

impl AddArgs {
    /// The words of the same `add` line in the PrzmTerm shell.
    fn words(&self) -> Vec<String> {
        let mut words = vec![self.name.clone(), self.target.clone()];
        for (flag, value) in [
            ("-f", &self.folder),
            ("-l", &self.user),
            ("-p", &self.port),
            ("-c", &self.credential),
            ("-i", &self.key),
            ("-J", &self.jump),
            ("-d", &self.description),
        ] {
            if let Some(v) = value {
                words.push(flag.into());
                words.push(v.clone());
            }
        }
        if self.replace {
            words.push("--replace".into());
        }
        words
    }
}

#[derive(clap::Args)]
pub struct DeleteArgs {
    /// The session: its name, or Folder/name.
    #[arg(add = ArgValueCompleter::new(complete_session))]
    pub session: Option<String>,
    /// With a session, the folder it is in; alone, the folder to delete
    /// with the sessions and folders in it.
    #[arg(short = 'f', long, value_name = "FOLDER", add = ArgValueCompleter::new(complete_folder))]
    pub folder: Option<String>,
    /// Delete without asking.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

fn config_dir(config: Option<PathBuf>) -> Result<PathBuf> {
    config
        .or_else(przmterm_session::default_config_dir)
        .context("no configuration directory found; pass --config")
}

/// The saved sessions by label and file, to find one `add` would write over.
fn known(dir: &Path) -> Vec<(String, PathBuf)> {
    SessionTree::load(dir)
        .root
        .all_sessions()
        .into_iter()
        .map(|s| (crate::label(s), s.path.clone()))
        .collect()
}

/// The vault's entry names, opened at most once, for `-c`.
struct VaultCheck {
    open: Option<Vec<String>>,
    tried: bool,
}

impl VaultCheck {
    fn new() -> Self {
        Self {
            open: None,
            tried: false,
        }
    }

    /// The entry `name` stands for, as the vault writes it. The vault is
    /// opened (master password from `PRZMTERM_VAULT_PASSWORD`, else asked)
    /// only for an entry no session uses yet.
    fn entry(&mut self, dir: &Path, name: &str, asker: &mut dyn Asker) -> Result<String> {
        let mut found = vault_entry(dir, name, self.open.as_deref());
        if found == Err(VaultMiss::Locked) && !self.tried {
            self.tried = true;
            let master = match std::env::var("PRZMTERM_VAULT_PASSWORD") {
                Ok(m) => Some(m),
                Err(_) => asker.ask(
                    &format!("Vault master password (to check -c {name}): "),
                    true,
                ),
            };
            if let Some(master) = master {
                let opened = vault::Vault::open(&vault::vault_path(dir), &master)
                    .map_err(|e| anyhow::anyhow!("vault: {e}"))?;
                self.open = Some(opened.names());
                found = vault_entry(dir, name, self.open.as_deref());
            }
        }
        found.map_err(|miss| {
            let hint = match &miss {
                VaultMiss::NoVault => "; make one in PrzmTerm (+ › Password vault…)",
                VaultMiss::Locked => {
                    "; set PRZMTERM_VAULT_PASSWORD, or run from a terminal, to open it"
                }
                VaultMiss::Unknown(_) => "",
            };
            anyhow::anyhow!("-c {name}: {}{hint}", miss.reason())
        })
    }
}

/// Adds the session `words` make: what it says when done.
fn add_words(
    dir: &Path,
    words: &[String],
    replace: bool,
    vault: &mut VaultCheck,
    asker: &mut dyn Asker,
) -> Result<String> {
    let mut spec = AddSpec::parse(words).map_err(anyhow::Error::msg)?;
    spec.replace |= replace;
    if let Some(name) = spec.settings.credential.clone().filter(|c| !c.is_empty()) {
        spec.settings.credential = Some(vault.entry(dir, &name, asker)?);
    }
    let shown = spec.shown.clone();
    let added = add_session(dir, spec, &known(dir)).map_err(anyhow::Error::msg)?;
    Ok(format!(
        "{} {}  {shown}",
        if added.replaced { "Replaced" } else { "Added" },
        added.label
    ))
}

/// `przm add`.
pub fn add(config: Option<PathBuf>, args: &AddArgs) -> Result<()> {
    let dir = config_dir(config)?;
    let said = add_words(
        &dir,
        &args.words(),
        false,
        &mut VaultCheck::new(),
        &mut TerminalAsker,
    )?;
    println!("{said}");
    Ok(())
}

/// `przm import`: the `add` lines of `file` (stdin for none or `-`).
/// Returns whether every line was added.
pub fn import(config: Option<PathBuf>, file: Option<&Path>, replace: bool) -> Result<bool> {
    use std::io::Read;
    let dir = config_dir(config)?;
    let text = match file.filter(|f| f != &Path::new("-")) {
        Some(f) => {
            std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?
        }
        None => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            text
        }
    };
    let mut vault = VaultCheck::new();
    let (mut added, mut failed) = (0, 0);
    for (n, line) in add_lines(&text) {
        let done = split_args(line)
            .map_err(anyhow::Error::msg)
            .and_then(|words| add_words(&dir, &words, replace, &mut vault, &mut TerminalAsker));
        match done {
            Ok(said) => {
                println!("{said}");
                added += 1;
            }
            Err(e) => {
                eprintln!("line {n}: {e:#}");
                failed += 1;
            }
        }
    }
    println!(
        "{added} session{} added{}.",
        plural(added),
        if failed > 0 {
            format!(", {failed} line{} not", plural(failed))
        } else {
            String::new()
        }
    );
    Ok(failed == 0)
}

/// `przm export`: the `add` lines of the saved sessions whose name,
/// address or description has `text`.
pub fn export(config: Option<PathBuf>, text: Option<&str>) -> Result<()> {
    let dir = config_dir(config)?;
    let tree = SessionTree::load(&dir);
    let wanted = text.unwrap_or("").trim().to_lowercase();
    let lines: Vec<String> = tree
        .root
        .all_sessions()
        .into_iter()
        .filter(|s| {
            wanted.is_empty()
                || crate::label(s).to_lowercase().contains(&wanted)
                || s.display_target().to_lowercase().contains(&wanted)
                || s.description
                    .as_deref()
                    .is_some_and(|d| d.to_lowercase().contains(&wanted))
        })
        .filter_map(export_line)
        .collect();
    if lines.is_empty() {
        bail!("no saved session to export");
    }
    println!(
        "# PrzmTerm {}: {} session{}. Add them with `przm import`, or paste the \
         lines into another PrzmTerm shell.",
        przmterm_core::VERSION,
        lines.len(),
        plural(lines.len())
    );
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

/// `przm delete`: asks first, unless `--yes`.
pub fn delete(config: Option<PathBuf>, args: &DeleteArgs) -> Result<()> {
    let dir = config_dir(config)?;
    let asker = &mut TerminalAsker;
    let folder = args
        .folder
        .as_deref()
        .map(przmterm_session::commands::clean_folder);
    let path = match (&folder, args.session.as_deref().map(str::trim)) {
        (None, None | Some("")) => {
            bail!("say what to delete: przm delete <session>, or -f <folder>")
        }
        (Some(f), None | Some("")) => return delete_folder(&dir, f, args.yes, asker),
        (Some(f), Some(name)) if !f.is_empty() => format!("{f}/{name}"),
        (_, Some(name)) => name.to_string(),
    };
    let tree = SessionTree::load(&dir);
    let sessions: Vec<przmterm_session::Session> =
        tree.root.all_sessions().into_iter().cloned().collect();
    let found = crate::saved_sessions(&sessions, &path);
    match found.as_slice() {
        [s] => {
            let label = crate::label(s);
            let question = format!(
                "Delete {label} ({} {})? Its settings file is removed. [y/N] ",
                s.protocol,
                s.display_target()
            );
            if confirm(&question, args.yes, asker)? {
                std::fs::remove_file(&s.path)
                    .with_context(|| format!("deleting {}", s.path.display()))?;
                println!("Deleted {label}.");
            } else {
                println!("Not deleted.");
            }
            Ok(())
        }
        [] if folder.is_none() && found_folder(&dir, &path).is_some() => {
            delete_folder(&dir, &path, args.yes, asker)
        }
        [] => bail!("{path}: no saved session by that name (przm sessions lists them)"),
        many => bail!(
            "several sessions are called {path}: {} (say which with Folder/name, or -f)",
            many.iter()
                .map(|s| crate::label(s))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn delete_folder(dir: &Path, path: &str, yes: bool, asker: &mut dyn Asker) -> Result<()> {
    let Some((at, label)) = found_folder(dir, path) else {
        bail!("{path}: no folder by that name");
    };
    let inside = match count_inside(&at) {
        (0, 0) => "It is empty.".to_string(),
        (s, 0) => format!("{s} session{} in it will be deleted too.", plural(s)),
        (0, f) => format!("{f} folder{} in it will be deleted too.", plural(f)),
        (s, f) => format!(
            "{s} session{} and {f} folder{} in it will be deleted too.",
            plural(s),
            plural(f)
        ),
    };
    if confirm(
        &format!("Delete the folder {label}? {inside} [y/N] "),
        yes,
        asker,
    )? {
        przmterm_session::delete_folder(&at).map_err(|e| anyhow::anyhow!(e.reason()))?;
        println!("Deleted the folder {label}.");
    } else {
        println!("Not deleted.");
    }
    Ok(())
}

fn confirm(question: &str, yes: bool, asker: &mut dyn Asker) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    let Some(answer) = asker.ask(question, false) else {
        bail!("nobody to answer \"{}\": add --yes", question.trim_end());
    };
    Ok(matches!(
        answer.trim().to_lowercase().as_str(),
        "y" | "yes" | "s" | "sim"
    ))
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

// Tab completion. The shell asks `przm` itself (COMPLETE=bash przm …);
// these read the settings folder `przm` would use.

/// The word as the shell has it so far: without an opening quote or
/// backslashes, and how to write candidates back the same way.
struct Typed {
    text: String,
    quote: Option<char>,
    /// bash puts a candidate back as it is: spaces need escaping there.
    /// zsh and fish quote what they insert.
    escape: bool,
}

impl Typed {
    fn of(current: &OsStr) -> Typed {
        let raw = current.to_string_lossy();
        let quote = raw.chars().next().filter(|c| matches!(c, '"' | '\''));
        let text = match quote {
            Some(q) => raw[1..].trim_end_matches(q).to_string(),
            None => raw.replace("\\ ", " "),
        };
        // Only bash's script passes its COMP_TYPE.
        let bash = std::env::var_os("_CLAP_COMPLETE_COMP_TYPE").is_some();
        Typed {
            text,
            quote,
            escape: bash,
        }
    }

    fn candidate(&self, value: &str, help: Option<String>) -> CompletionCandidate {
        let written = match (self.escape, self.quote) {
            (false, _) => value.to_string(),
            (true, Some(q)) => format!("{q}{value}"),
            (true, None) => value.replace(' ', "\\ "),
        };
        CompletionCandidate::new(written).help(help.map(Into::into))
    }
}

fn completion_dir() -> Option<PathBuf> {
    przmterm_session::default_config_dir()
}

/// The folders under `parent` (`A/B`, letter case aside) whose name starts
/// with `start`: their paths as written, and whether something is inside.
fn folders_at(dir: &Path, parent: &str, start: &str) -> Vec<(String, bool)> {
    let parent = przmterm_session::commands::existing_folder(dir, parent);
    let mut at = dir.join(przmterm_session::SESSIONS_DIR);
    for part in parent.split('/').filter(|p| !p.is_empty()) {
        at.push(part);
    }
    let start = start.to_lowercase();
    let mut found: Vec<(String, bool)> = subfolders(&at)
        .into_iter()
        .filter(|name| name.to_lowercase().starts_with(&start))
        .map(|name| {
            let inside = !subfolders(&at.join(&name)).is_empty();
            let path = if parent.is_empty() {
                name
            } else {
                format!("{parent}/{name}")
            };
            (path, inside)
        })
        .collect();
    found.sort_by_key(|(p, _)| p.to_lowercase());
    found
}

/// `-f`: folders a level at a time; one with folders in it ends in `/`.
fn complete_folder(current: &OsStr) -> Vec<CompletionCandidate> {
    let typed = Typed::of(current);
    let Some(dir) = completion_dir() else {
        return Vec::new();
    };
    let (parent, start) = typed.text.rsplit_once('/').unwrap_or(("", &typed.text));
    folders_at(&dir, parent, start)
        .into_iter()
        .map(|(path, inside)| {
            let value = if inside { format!("{path}/") } else { path };
            typed.candidate(&value, None)
        })
        .collect()
}

/// A session: by name anywhere, or by Folder/name a level at a time.
pub(crate) fn complete_session(current: &OsStr) -> Vec<CompletionCandidate> {
    let typed = Typed::of(current);
    let Some(dir) = completion_dir() else {
        return Vec::new();
    };
    let tree = SessionTree::load(&dir);
    let wanted = typed.text.to_lowercase();
    let mut out: Vec<CompletionCandidate> = Vec::new();
    let (parent, start) = typed.text.rsplit_once('/').unwrap_or(("", &typed.text));
    // The folders, then the sessions, in the folder being typed.
    for (path, _) in folders_at(&dir, parent, start) {
        out.push(typed.candidate(&format!("{path}/"), None));
    }
    let parent_lower = parent.to_lowercase();
    for s in tree.root.all_sessions() {
        let label = crate::label(s);
        let help = Some(format!("{} {}", s.protocol, s.display_target()));
        if s.folder.join("/").to_lowercase() == parent_lower
            && s.name.to_lowercase().starts_with(&start.to_lowercase())
        {
            out.push(typed.candidate(&label, help));
        } else if !typed.text.contains('/') && s.name.to_lowercase().starts_with(&wanted) {
            // By its name alone, as `przm run` and `delete` take it.
            out.push(typed.candidate(&s.name, help));
        }
    }
    out
}

/// `-c`: the vault entries the sessions use (the vault's names are
/// encrypted; nobody types the master password to complete a word).
fn complete_credential(current: &OsStr) -> Vec<CompletionCandidate> {
    let typed = Typed::of(current);
    let Some(dir) = completion_dir() else {
        return Vec::new();
    };
    let start = typed.text.to_lowercase();
    vault::names_in_use(&dir)
        .into_iter()
        .filter(|n| n.to_lowercase().starts_with(&start))
        .map(|n| typed.candidate(&n, None))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers each question with the next of its answers.
    struct Answers(Vec<&'static str>);

    impl Asker for Answers {
        fn ask(&mut self, _: &str, _: bool) -> Option<String> {
            (!self.0.is_empty()).then(|| self.0.remove(0).to_string())
        }
    }

    #[test]
    fn adds_checks_the_vault_and_deletes() {
        let dir = std::env::temp_dir().join(format!("przm-manage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let words = |line: &str| split_args(line).unwrap();
        let mut vault = VaultCheck::new();
        let no_one = &mut Answers(Vec::new());
        let said = add_words(
            &dir,
            &words("r1 10.0.0.1 -f \"Data Center/Net\""),
            false,
            &mut vault,
            no_one,
        )
        .unwrap();
        assert_eq!(said, "Added Data Center/Net/r1  ssh 10.0.0.1:22");
        let err = add_words(&dir, &words("r2 10.0.0.2 -c x"), false, &mut vault, no_one);
        assert!(err.unwrap_err().to_string().contains("no password vault"));

        // A real vault, opened once for an entry no session uses.
        let path = vault::vault_path(&dir);
        let mut v =
            vault::Vault::create(&path, "master-password", vault::KdfCost::for_tests()).unwrap();
        v.entries.push(vault::VaultEntry {
            name: "Lab Admin".into(),
            username: "admin".into(),
            password: String::new(),
            passphrase: String::new(),
        });
        v.save().unwrap();
        let err = add_words(
            &dir,
            &words("r2 10.0.0.2 -c \"lab admin\""),
            false,
            &mut VaultCheck::new(),
            no_one,
        );
        assert!(err.unwrap_err().to_string().contains("locked"));
        let mut vault = VaultCheck::new();
        let asks = &mut Answers(vec!["master-password"]);
        add_words(
            &dir,
            &words("r2 10.0.0.2 -c \"lab admin\""),
            false,
            &mut vault,
            asks,
        )
        .unwrap();
        let err = add_words(
            &dir,
            &words("r3 10.0.0.3 -c nobody"),
            false,
            &mut vault,
            no_one,
        );
        assert!(err.unwrap_err().to_string().contains("it has Lab Admin"));
        // Used by r2 now: no need to open the vault.
        add_words(
            &dir,
            &words("r3 10.0.0.3 -c \"LAB ADMIN\""),
            false,
            &mut VaultCheck::new(),
            no_one,
        )
        .unwrap();
        let tree = SessionTree::load(&dir);
        let r3 = tree
            .root
            .all_sessions()
            .into_iter()
            .find(|s| s.name == "r3")
            .cloned();
        assert_eq!(r3.and_then(|s| s.credential).as_deref(), Some("Lab Admin"));

        // Completion, a level at a time and letter case aside.
        assert_eq!(
            folders_at(&dir, "", "da"),
            [("Data Center".to_string(), true)]
        );
        assert_eq!(
            folders_at(&dir, "data center", "n"),
            [("Data Center/Net".to_string(), false)]
        );

        assert!(delete_folder(&dir, "data center", false, &mut Answers(vec!["n"])).is_ok());
        assert!(dir.join("sessions/Data Center").is_dir());
        assert!(delete_folder(&dir, "data center", false, no_one).is_err());
        delete_folder(&dir, "data center", true, no_one).unwrap();
        assert!(!dir.join("sessions/Data Center").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
