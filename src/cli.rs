//! Command-line interface definition (clap derive).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::format::Format;

#[derive(Parser, Debug)]
#[command(
    name = "redink",
    version,
    about = "A spellchecker for prose — a TUI for humans, a JSON CLI for agents"
)]
pub struct Cli {
    #[command(flatten)]
    pub opts: GlobalOpts,

    /// Files to check with no subcommand given (launches the TUI when
    /// interactive, otherwise a non-interactive check). With none given, walks
    /// the current directory for text/markdown.
    #[arg(value_name = "FILES")]
    pub files: Vec<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(clap::Args, Debug)]
pub struct GlobalOpts {
    /// Override the working-dictionary path (default: nearest `.redink.dic`).
    #[arg(long, global = true)]
    pub dict: Option<PathBuf>,

    /// Dictionary language, e.g. en_US, en_GB (default: en_US).
    #[arg(long, global = true, default_value = "en_US")]
    pub lang: String,

    /// Directory containing `<lang>.aff` / `<lang>.dic` (otherwise searched).
    #[arg(long, global = true)]
    pub sysdict_dir: Option<PathBuf>,

    /// Input format for files without a recognizable extension.
    #[arg(long, global = true, value_enum, default_value_t = Format::Auto)]
    pub format: Format,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Non-interactive check. Prints results to stdout.
    Check {
        /// Files to check. With none, walks the cwd for text/markdown.
        files: Vec<PathBuf>,

        /// Emit JSON (one object per occurrence) for programmatic use.
        #[arg(long)]
        json: bool,

        /// Print only unique misspelled words, one per line.
        #[arg(long)]
        words: bool,
    },

    /// Interactive terminal UI (list + detail, with suggestions).
    Tui {
        /// Files to check. With none, walks the cwd for text/markdown.
        files: Vec<PathBuf>,
    },

    /// Manage the working dictionary.
    Dict {
        #[command(subcommand)]
        action: DictAction,
    },

    /// Replace the word at a byte offset (non-interactive; for agents).
    Fix {
        /// The file to edit in place.
        file: PathBuf,
        /// Absolute byte offset of the word to replace (see `check --json`).
        #[arg(long)]
        at: usize,
        /// The word currently at that offset (validated before replacing).
        #[arg(long)]
        word: String,
        /// The replacement text.
        #[arg(long)]
        to: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum DictAction {
    /// List the words in the working dictionary.
    List,
    /// Add words. Case-insensitive by default; use --sensitive for exact case.
    Add {
        words: Vec<String>,
        #[arg(short = 's', long)]
        sensitive: bool,
    },
    /// Remove words (matches either case layer).
    Remove { words: Vec<String> },
}
