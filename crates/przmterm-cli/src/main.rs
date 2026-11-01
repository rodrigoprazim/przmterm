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
    /// Configuration directory (default: $PRZMTERM_HOME or the OS config dir).
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Sessions => sessions(cli.config),
        Command::Render { file, cols, rows } => render(file, cols, rows),
    }
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
