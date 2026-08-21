use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

mod check;
mod cli;
// Shared with `build.rs` (via `#[path]`); test-only here — `cargo test`
// doesn't run build-script tests, so the patcher's unit tests live here.
mod dict;
#[cfg(test)]
mod dictpatch;
mod engine;
mod format;
mod report;
mod sysdict;
mod token;
mod tui;

use cli::{Command, DictAction};
use engine::Engine;

const SUGGEST_LIMIT: usize = 9;

fn main() -> ExitCode {
    let args = cli::Cli::parse();
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("redink: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(args: cli::Cli) -> Result<ExitCode> {
    let cli::Cli {
        opts,
        command,
        files,
    } = args;

    match command {
        Some(Command::Fix { file, at, word, to }) => {
            apply_fix(&file, at, &word, &to)?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Dict { action }) => {
            run_dict(&opts, action)?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Check { files, json, words }) => {
            let engine = build_engine(&opts)?;
            let needs_suggestions = !words;
            let miss = check_all(&files, opts.format, &engine, needs_suggestions)?;
            let found = !miss.is_empty();
            let mut out = std::io::stdout().lock();
            if json {
                report::write_json(&mut out, &miss)?;
            } else if words {
                report::write_words(&mut out, &miss)?;
            } else {
                report::write_text(&mut out, &miss)?;
            }
            if found && !json {
                eprintln!("{}", report::summary(&miss));
            }
            Ok(if found {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        Some(Command::Tui { files }) => {
            let engine = build_engine(&opts)?;
            let miss = check_all(&files, opts.format, &engine, false)?;
            tui::run(miss, engine)?;
            Ok(ExitCode::SUCCESS)
        }
        None => {
            // No subcommand: files given on the command line (or, if none,
            // everything checkable in the cwd). TUI if interactive, else check.
            let files = if files.is_empty() {
                discover_files()
            } else {
                files
            };
            if std::io::stdout().is_terminal() {
                let engine = build_engine(&opts)?;
                let miss = check_all(&files, opts.format, &engine, false)?;
                tui::run(miss, engine)?;
                Ok(ExitCode::SUCCESS)
            } else {
                let engine = build_engine(&opts)?;
                let miss = check_all(&files, opts.format, &engine, true)?;
                let found = !miss.is_empty();
                let mut out = std::io::stdout().lock();
                report::write_text(&mut out, &miss)?;
                if found {
                    eprintln!("{}", report::summary(&miss));
                }
                Ok(if found {
                    ExitCode::from(1)
                } else {
                    ExitCode::SUCCESS
                })
            }
        }
    }
}

fn build_engine(opts: &cli::GlobalOpts) -> Result<Engine> {
    let sys = sysdict::resolve(&opts.lang, opts.sysdict_dir.as_deref())?;
    let dict = engine::load_dictionary(&sys.aff, &sys.dic)?;
    let working_path = dict::locate(opts.dict.as_deref());
    let working = dict::load(&working_path)?;
    let engine = Engine::new(dict, working, working_path);
    eprintln!(
        "[dict: {} | working: {} ({} words)]",
        sys.source,
        engine.working_path().display(),
        engine.working_ci_count() + engine.working_cs_count(),
    );
    Ok(engine)
}

use rayon::prelude::*;

fn check_all(
    files: &[PathBuf],
    fmt: format::Format,
    engine: &Engine,
    needs_suggestions: bool,
) -> Result<Vec<check::Misspelling>> {
    let files = resolve_files(files);

    // Step 1: Check all files in parallel, initially without computing suggestions
    // (since suggestion generation is the slow part and we want to deduplicate it).
    let mut miss: Vec<check::Misspelling> = files
        .into_par_iter()
        .flat_map(|f| {
            // We pass a dummy cache since needs_suggestions=false means it won't be used.
            let mut cache = std::collections::HashMap::new();
            match check::check_file(&f, fmt, engine, &mut cache, SUGGEST_LIMIT, false) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("redink: skipping {}: {e}", f.display());
                    Vec::new()
                }
            }
        })
        .collect();

    // Step 2: If suggestions are needed, gather unique misspelled words,
    // compute their suggestions in parallel, and attach them.
    if needs_suggestions && !miss.is_empty() {
        let unique_words: std::collections::HashSet<String> =
            miss.iter().map(|m| m.word.clone()).collect();

        let suggest_cache: std::collections::HashMap<String, Vec<String>> = unique_words
            .into_par_iter()
            .map(|word| {
                let sugs = engine
                    .suggest(&word)
                    .into_iter()
                    .take(SUGGEST_LIMIT)
                    .collect();
                (word, sugs)
            })
            .collect();

        for m in &mut miss {
            if let Some(sugs) = suggest_cache.get(&m.word) {
                m.suggestions = Some(sugs.clone());
            }
        }
    }

    Ok(miss)
}

/// Expand the user's file arguments, or discover them when none are given.
fn resolve_files(files: &[PathBuf]) -> Vec<PathBuf> {
    if !files.is_empty() {
        files.to_vec()
    } else {
        discover_files()
    }
}

fn discover_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for result in ignore::Walk::new(".") {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let p = entry.path();
        if is_checkable_ext(p) {
            out.push(p.to_path_buf());
        }
    }
    out.sort();
    out
}

fn is_checkable_ext(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("md" | "markdown" | "mdown" | "mkd" | "txt" | "text")
    )
}

fn run_dict(opts: &cli::GlobalOpts, action: DictAction) -> Result<()> {
    let working_path = dict::locate(opts.dict.as_deref());
    match action {
        DictAction::List => {
            let d = dict::load(&working_path)?;
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "# {}", working_path.display());
            let mut ci: Vec<&String> = d.ci.iter().collect();
            ci.sort();
            let mut cs: Vec<&String> = d.cs.iter().collect();
            cs.sort();
            let mut ph: Vec<&String> = d.phrases.iter().collect();
            ph.sort();
            let mut phcs: Vec<&String> = d.phrases_cs.iter().collect();
            phcs.sort();
            for w in ci {
                let _ = writeln!(out, "{w}");
            }
            for w in cs {
                let _ = writeln!(out, "={w}");
            }
            if !ph.is_empty() || !phcs.is_empty() {
                let _ = writeln!(out, "# phrases:");
                for w in ph {
                    let _ = writeln!(out, "{w}");
                }
                for w in phcs {
                    let _ = writeln!(out, "={w}");
                }
            }
        }
        DictAction::Add { words, sensitive } => {
            let mut d = dict::load(&working_path)?;
            let mut added = 0usize;
            let mut duplicates: Vec<String> = Vec::new();
            for w in &words {
                match d.add_entry(w, sensitive) {
                    dict::AddOutcome::Added => added += 1,
                    dict::AddOutcome::Duplicate => duplicates.push(w.clone()),
                    dict::AddOutcome::Ignored => {}
                }
            }
            dict::save(&working_path, &d)?;
            if added != 0 {
                eprintln!(
                    "added {} entr{} to {}",
                    added,
                    if added == 1 { "y" } else { "ies" },
                    working_path.display()
                );
            }
            if !duplicates.is_empty() {
                eprintln!("already present: {}", duplicates.join(", "));
            }
        }
        DictAction::Remove { words } => {
            let mut d = dict::load(&working_path)?;
            let mut removed = 0;
            for w in &words {
                if d.remove(w) {
                    removed += 1;
                }
            }
            dict::save(&working_path, &d)?;
            eprintln!("removed {removed} word(s) from {}", working_path.display());
        }
    }
    Ok(())
}

fn apply_fix(file: &Path, at: usize, word: &str, to: &str) -> Result<()> {
    let src = std::fs::read(file)?;
    let end = at + word.len();
    if at > src.len() || end > src.len() {
        anyhow::bail!(
            "byte range [{at}, {end}) is outside {} ({} bytes)",
            file.display(),
            src.len()
        );
    }
    if &src[at..end] != word.as_bytes() {
        anyhow::bail!(
            "word at offset {at} is {:?}, not {word:?} (file may have changed)",
            std::str::from_utf8(&src[at..end]).unwrap_or("<invalid utf-8>")
        );
    }
    let mut out = Vec::with_capacity(src.len() - word.len() + to.len());
    out.extend_from_slice(&src[..at]);
    out.extend_from_slice(to.as_bytes());
    out.extend_from_slice(&src[end..]);
    std::fs::write(file, out)?;
    Ok(())
}
