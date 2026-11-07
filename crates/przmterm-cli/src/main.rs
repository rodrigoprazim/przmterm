//! `przm`: the PrzmTerm command-line interface.

use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use przmterm_session::{Folder, SessionTree};
use przmterm_term::Terminal;

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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Sessions => sessions(cli.config),
        Command::Render { file, cols, rows } => render(file, cols, rows),
        Command::ImportSecurecrt {
            paths,
            into,
            dry_run,
        } => import_securecrt(cli.config, &paths, &into, dry_run),
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
