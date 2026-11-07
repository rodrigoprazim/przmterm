//! `przm`: the PrzmTerm command-line interface.

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use przmterm_session::{Folder, SessionTree};
use przmterm_term::Terminal;

mod run;
use run::Asker;

#[derive(Parser)]
#[command(name = "przm", version = przmterm_core::VERSION, about = "PrzmTerm command-line interface")]
struct Cli {
    /// Configuration directory (default: $PRZMTERM_HOME, the folder chosen in
    /// the app, or the OS config dir).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Lists saved sessions as a tree.
    Sessions,
    /// Feeds bytes (a file or stdin) through the emulator and prints the screen.
    Render {
        /// File with raw terminal output; reads stdin when omitted.
        file: Option<PathBuf>,
        #[arg(long, default_value_t = 80)]
        cols: usize,
        #[arg(long, default_value_t = 24)]
        rows: usize,
    },
    /// Imports SecureCRT sessions: an XML export, session .ini files, or a
    /// Sessions folder. Passwords are not imported.
    ImportSecurecrt {
        /// XML export, .ini files or folders.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Folder of the session tree to import into ("" for the top level).
        #[arg(long, default_value = "SecureCRT")]
        into: String,
        /// Show what would be imported without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Connects a session without a window, runs commands and prints the
    /// output. Commands come from --cmd, else one per line from stdin.
    ///
    /// Logs in with the session's key, vault entry or the password saved in
    /// the keychain; other login questions are asked on the terminal.
    Run {
        /// A saved session (its name, or Folder/name), or user@host[:port].
        session: String,
        /// A command to send; repeat for several, run in order.
        #[arg(short, long = "cmd", value_name = "COMMAND")]
        commands: Vec<String>,
        /// Seconds to wait for the login and for each command's prompt.
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        /// The prompt, as a regular expression the last line matches.
        #[arg(long, default_value = run::DEFAULT_PROMPT)]
        prompt: String,
        /// Milliseconds of quiet after a prompt before a command counts as done.
        #[arg(long, default_value_t = 400)]
        settle: u64,
        /// Accept a host key seen for the first time (and remember it).
        #[arg(long)]
        accept_host_key: bool,
        /// Environment variable holding the password for password prompts.
        #[arg(long, value_name = "VAR")]
        password_env: Option<String>,
        /// Print the output as received, escape sequences included.
        #[arg(long)]
        raw: bool,
        /// Show the login, and SSH's steps, on stderr.
        #[arg(short, long)]
        verbose: bool,
    },
}

fn main() {
    // A plain message, not a backtrace, when something goes wrong.
    if let Err(e) = real_main() {
        eprintln!("przm: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Sessions => sessions(cli.config),
        Command::Render { file, cols, rows } => render(file, cols, rows),
        Command::ImportSecurecrt {
            paths,
            into,
            dry_run,
        } => import_securecrt(cli.config, &paths, &into, dry_run),
        Command::Run {
            session,
            commands,
            timeout,
            prompt,
            settle,
            accept_host_key,
            password_env,
            raw,
            verbose,
        } => {
            let password = match password_env {
                Some(var) => {
                    Some(std::env::var(&var).with_context(|| format!("${var} is not set"))?)
                }
                None => None,
            };
            let opts = run::Options {
                commands,
                prompt: fancy_regex::Regex::new(&prompt)
                    .with_context(|| format!("--prompt `{prompt}`"))?,
                timeout: std::time::Duration::from_secs(timeout),
                settle: std::time::Duration::from_millis(settle),
                accept_host_key,
                raw,
                verbose,
                password,
            };
            let code = run_session(cli.config, &session, opts)?;
            std::process::exit(code);
        }
    }
}

/// `przm run`: returns the exit code (0 done, 2 a prompt did not come back).
fn run_session(config: Option<PathBuf>, target: &str, mut opts: run::Options) -> Result<i32> {
    use std::io::{BufRead, IsTerminal};
    let dir = config.or_else(przmterm_session::default_config_dir);
    let tree = dir.as_deref().map(SessionTree::load).unwrap_or_default();
    let sessions: Vec<przmterm_session::Session> =
        tree.root.all_sessions().into_iter().cloned().collect();
    let mut session = find_session(&sessions, target)?;
    if opts.verbose {
        session.debug = true;
    }
    if opts.commands.is_empty() {
        if std::io::stdin().is_terminal() {
            bail!("nothing to run: give --cmd, or pipe the commands in");
        }
        for line in std::io::stdin().lock().lines() {
            let line = line?;
            if !line.trim().is_empty() {
                opts.commands.push(line);
            }
        }
    }
    let mut env = przmterm_connect::Env {
        known_hosts: dir
            .clone()
            .unwrap_or_else(std::env::temp_dir)
            .join(przmterm_session::KNOWN_HOSTS_FILE),
        config_dir: dir.clone(),
        sessions,
        credentials: None,
        headless: true,
    };
    // Vault entries need the master password.
    if !przmterm_connect::vault_entries_used(&session, &env).is_empty()
        && let Some(dir) = &dir
    {
        let path = przmterm_session::vault::vault_path(dir);
        let master = match std::env::var("PRZMTERM_VAULT_PASSWORD") {
            Ok(m) => m,
            Err(_) => run::TerminalAsker
                .ask("Vault master password: ", true)
                .context(
                    "the session logs in with a vault entry: set PRZMTERM_VAULT_PASSWORD \
                     or run from a terminal",
                )?,
        };
        let vault = przmterm_session::vault::Vault::open(&path, &master)
            .map_err(|e| anyhow::anyhow!("vault: {e}"))?;
        env.credentials = Some(std::sync::Arc::new(vault.entries.clone()));
    }
    let (waker, wait) = run::new_waker();
    let mut transport = przmterm_connect::connect(&session, &env, run::SIZE, waker);
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let ending = run::drive(
        transport.as_mut(),
        &session.triggers,
        &opts,
        &mut run::TerminalAsker,
        &wait,
        &mut stdout.lock(),
        &mut stderr.lock(),
    );
    let _ = transport.close();
    match ending? {
        run::Ending::Done => Ok(0),
        run::Ending::TimedOut(cmd) => {
            eprintln!(
                "przm: no prompt within {} s after `{cmd}`",
                opts.timeout.as_secs()
            );
            Ok(2)
        }
    }
}

/// A saved session by Folder/name or name (case does not matter), else a
/// quick-connect target.
fn find_session(
    sessions: &[przmterm_session::Session],
    target: &str,
) -> Result<przmterm_session::Session> {
    let label = |s: &przmterm_session::Session| {
        let mut l = s.folder.join("/");
        if !l.is_empty() {
            l.push('/');
        }
        l.push_str(&s.name);
        l
    };
    let wanted = target.trim().trim_end_matches('/');
    let by_label: Vec<_> = sessions
        .iter()
        .filter(|s| label(s).eq_ignore_ascii_case(wanted))
        .collect();
    let found = if by_label.is_empty() {
        sessions
            .iter()
            .filter(|s| s.name.eq_ignore_ascii_case(wanted))
            .collect()
    } else {
        by_label
    };
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => przmterm_session::Session::quick_connect(wanted)
            .map_err(|e| anyhow::anyhow!("`{target}` is not a saved session, nor a host: {e}")),
        many => bail!(
            "several sessions are called {target}: {}",
            many.iter().map(|s| label(s)).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn import_securecrt(
    config: Option<PathBuf>,
    paths: &[PathBuf],
    into: &str,
    dry_run: bool,
) -> Result<()> {
    use przmterm_session::securecrt;
    let report = securecrt::parse_any(paths).map_err(anyhow::Error::msg)?;
    for s in &report.sessions {
        let folder = std::iter::once(into)
            .filter(|f| !f.is_empty())
            .chain(s.folder.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("/");
        let target = s
            .settings
            .host
            .clone()
            .or_else(|| s.settings.serial_port.clone())
            .unwrap_or_default();
        let via = s
            .settings
            .jump_host
            .as_ref()
            .map(|j| format!("  via {j}"))
            .unwrap_or_default();
        println!("{folder}/{}  {target}{via}", s.name);
    }
    for (name, set) in &report.keyword_sets {
        println!("keyword set {name}: {} rule(s)", set.rules.len());
    }
    for (name, why) in &report.skipped {
        println!("skipped {name}: {why}");
    }
    for note in &report.notes {
        println!("note: {note}");
    }
    if dry_run {
        println!(
            "{} session(s) would be imported (dry run).",
            report.sessions.len()
        );
        return Ok(());
    }
    let dir = config
        .or_else(przmterm_session::default_config_dir)
        .context("no configuration directory found; pass --config")?;
    let n = securecrt::write(&dir, into, &report).map_err(anyhow::Error::msg)?;
    println!(
        "Imported {n} session(s) into {}.",
        dir.join(przmterm_session::SESSIONS_DIR)
            .join(into)
            .display()
    );
    Ok(())
}

fn sessions(config: Option<PathBuf>) -> Result<()> {
    let dir = config
        .or_else(przmterm_session::default_config_dir)
        .context("no configuration directory found; pass --config")?;
    let tree = SessionTree::load(&dir);
    if tree.root.all_sessions().is_empty() {
        println!(
            "No sessions in {}",
            dir.join(przmterm_session::SESSIONS_DIR).display()
        );
    } else {
        print_folder(&tree.root, 0);
    }
    for err in &tree.errors {
        eprintln!("warning: {err}");
    }
    Ok(())
}

fn print_folder(folder: &Folder, depth: usize) {
    let indent = "  ".repeat(depth);
    for f in &folder.folders {
        println!("{indent}{}/", f.name);
        print_folder(f, depth + 1);
    }
    for s in &folder.sessions {
        println!(
            "{indent}{}  [{}] {}",
            s.name,
            s.protocol,
            s.display_target()
        );
    }
}

fn render(file: Option<PathBuf>, cols: usize, rows: usize) -> Result<()> {
    let bytes = match file {
        Some(path) => {
            std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?
        }
        None => {
            let mut buf = Vec::new();
            std::io::stdin().read_to_end(&mut buf)?;
            buf
        }
    };
    let mut term = Terminal::new(cols, rows);
    term.feed(&bytes);
    println!("{}", term.screen().text());
    Ok(())
}
