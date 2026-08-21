use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;
use rayon::prelude::*;

mod check;
mod cli;
// Shared with `build.rs` (via `#[path]`); test-only here — `cargo test`
// doesn't run build-script tests, so the patcher's unit tests live here.
mod dict;
#[cfg(test)]
mod dictpatch;
mod engine;
mod format;
mod fsutil;
mod report;
mod sysdict;
#[cfg(test)]
mod testutil;
mod token;
mod tui;

use cli::{Command, DictAction};
use engine::{Engine, SUGGEST_LIMIT, Suggest};

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

    // No subcommand: TUI when interactive, otherwise a plain text check.
    let command = match command {
        Some(c) => c,
        None if std::io::stdout().is_terminal() => Command::Tui { files },
        None => Command::Check {
            files,
            json: false,
            words: false,
        },
    };

    match command {
        Command::Fix { file, at, word, to } => {
            apply_fix(&file, at, &word, &to)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Dict { action } => {
            run_dict(&opts, action)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Check { files, json, words } => {
            let engine = build_engine(&opts)?;
            let needs_suggestions = !words;
            let scan = check_all(&files, opts.format, &engine, needs_suggestions)?;
            let miss = &scan.misspellings;
            let found = !miss.is_empty();
            let mut out = std::io::stdout().lock();
            if json {
                report::write_json(&mut out, miss)?;
            } else if words {
                report::write_words(&mut out, miss)?;
            } else {
                report::write_text(&mut out, miss)?;
            }
            drop(out);
            scan.report_failures();
            if found && !json {
                eprintln!("{}", report::summary(miss));
            }
            // An unreadable file means the answer is incomplete. Reporting
            // "clean" would be a lie, and `check --json` is what agents trust.
            Ok(match (scan.failed, found) {
                (true, _) => ExitCode::from(2),
                (false, true) => ExitCode::from(1),
                (false, false) => ExitCode::SUCCESS,
            })
        }
        Command::Tui { files } => {
            let engine = build_engine(&opts)?;
            let scan = check_all(&files, opts.format, &engine, false)?;
            scan.report_failures();
            tui::run(scan.misspellings, engine, opts.format)?;
            Ok(ExitCode::SUCCESS)
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
        engine.working_word_count(),
    );
    Ok(engine)
}

/// The result of a check pass: what was found, and what could not be read.
struct Scan {
    misspellings: Vec<check::Misspelling>,
    /// One message per file that could not be checked.
    errors: Vec<String>,
    /// Whether any file failed. Kept separate so callers can decide an exit
    /// code after `errors` has been consumed by reporting.
    failed: bool,
}

impl Scan {
    /// Print the unreadable-file messages to stderr, if any.
    fn report_failures(&self) {
        for e in &self.errors {
            eprintln!("redink: {e}");
        }
    }
}

fn check_all(
    files: &[PathBuf],
    fmt: format::Format,
    engine: &Engine,
    needs_suggestions: bool,
) -> Result<Scan> {
    let files = resolve_files(files);

    // Step 1: Check all files in parallel (suggestions, the slow part, come
    // later so they can be deduplicated).
    let results: Vec<Result<Vec<check::Misspelling>, String>> = files
        .into_par_iter()
        .map(|f| check::check_file(&f, fmt, engine).map_err(|e| format!("{}: {e}", f.display())))
        .collect();

    let mut miss: Vec<check::Misspelling> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for result in results {
        match result {
            Ok(found) => miss.extend(found),
            Err(e) => errors.push(e),
        }
    }

    // Step 2: If suggestions are needed, gather unique misspelled words,
    // compute their suggestions in parallel, and attach them.
    if needs_suggestions && !miss.is_empty() {
        let unique_words: std::collections::HashSet<String> =
            miss.iter().map(|m| m.word.clone()).collect();

        let suggest_cache: std::collections::HashMap<String, Vec<String>> = unique_words
            .into_par_iter()
            .map(|word| {
                let sugs = engine
                    .suggest(&word, Suggest::Fast)
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

    Ok(Scan {
        misspellings: miss,
        failed: !errors.is_empty(),
        errors,
    })
}

/// Expand the user's file arguments, or discover them when none are given.
/// Directory arguments are walked the same way a bare `redink check` walks the
/// current directory; explicit file arguments are taken as given, extension or
/// not, since naming a file is an unambiguous request to check it.
fn resolve_files(files: &[PathBuf]) -> Vec<PathBuf> {
    if files.is_empty() {
        return discover_files(Path::new("."));
    }
    let mut out = Vec::new();
    for f in files {
        if f.is_dir() {
            out.extend(discover_files(f));
        } else {
            out.push(f.clone());
        }
    }
    out
}

/// Walk `root` for checkable files, honoring `.gitignore` and friends.
fn discover_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for result in ignore::Walk::new(root) {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let p = entry.path();
        if format::Format::is_checkable(p) {
            out.push(p.to_path_buf());
        }
    }
    out.sort();
    out
}

fn run_dict(opts: &cli::GlobalOpts, action: DictAction) -> Result<()> {
    let working_path = dict::locate(opts.dict.as_deref());
    match action {
        DictAction::List => {
            let d = dict::load(&working_path)?;
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "# {}", working_path.display());
            let _ = write!(out, "{}", d.render_body());
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
            // Only touch the file when something actually changed: the working
            // dictionary is committed to the repo, and a rewrite that changes
            // nothing is just noise in someone's diff.
            if added != 0 {
                dict::save(&working_path, &d)?;
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
            if removed != 0 {
                dict::save(&working_path, &d)?;
            }
            eprintln!("removed {removed} word(s) from {}", working_path.display());
        }
    }
    Ok(())
}

fn apply_fix(file: &Path, at: usize, word: &str, to: &str) -> Result<()> {
    let src = std::fs::read(file)?;
    // `at` comes straight off the command line, so the addition is checked:
    // an absurd offset should be a clean error, not an overflow panic.
    let end = match at.checked_add(word.len()) {
        Some(end) if end <= src.len() => end,
        _ => anyhow::bail!(
            "byte range [{at}, {at}+{}) is outside {} ({} bytes)",
            word.len(),
            file.display(),
            src.len()
        ),
    };
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
    fsutil::write_atomic(file, &out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn test_engine() -> Engine {
        let sys = sysdict::resolve_embedded();
        let dict = engine::load_dictionary(&sys.aff, &sys.dic).unwrap();
        Engine::new(
            dict,
            dict::WorkingDict::default(),
            PathBuf::from("/dev/null"),
        )
    }

    #[test]
    fn apply_fix_replaces_exactly_the_named_bytes() {
        let s = testutil::scratch("fix");
        let p = s.path("a.md");
        std::fs::write(&p, "Hello wrold today\n").unwrap();
        apply_fix(&p, 6, "wrold", "world").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "Hello world today\n");
    }

    #[test]
    fn apply_fix_refuses_when_the_word_moved() {
        let s = testutil::scratch("fix-moved");
        let p = s.path("a.md");
        std::fs::write(&p, "Hello world today\n").unwrap();
        let err = apply_fix(&p, 6, "wrold", "world").unwrap_err();
        assert!(err.to_string().contains("not"), "{err}");
        // The file must be untouched after a refusal.
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "Hello world today\n");
    }

    /// An offset past the end is a clean error — including one large enough to
    /// overflow the end calculation.
    #[test]
    fn apply_fix_rejects_out_of_range_offsets() {
        let s = testutil::scratch("fix-range");
        let p = s.path("a.md");
        std::fs::write(&p, "short\n").unwrap();
        assert!(apply_fix(&p, 100, "wrold", "world").is_err());
        assert!(apply_fix(&p, usize::MAX, "wrold", "world").is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "short\n");
    }

    /// A directory argument is walked, the same way a bare `redink check`
    /// walks the current directory. Passing one used to check nothing at all.
    #[test]
    fn resolve_files_walks_directory_arguments() {
        let s = testutil::scratch("walk");
        std::fs::create_dir_all(s.path("sub")).unwrap();
        std::fs::write(s.path("sub").join("a.md"), "x").unwrap();
        std::fs::write(s.path("sub").join("b.txt"), "x").unwrap();
        std::fs::write(s.path("sub").join("c.png"), "x").unwrap();

        let found = resolve_files(&[s.path("sub")]);
        let names: Vec<String> = found
            .iter()
            .filter_map(|p| p.file_name()?.to_str().map(String::from))
            .collect();
        assert!(names.contains(&"a.md".to_string()), "{names:?}");
        assert!(names.contains(&"b.txt".to_string()), "{names:?}");
        assert!(!names.contains(&"c.png".to_string()), "{names:?}");

        // An explicitly named file is taken as given, extension or not.
        let named = resolve_files(&[s.path("sub").join("c.png")]);
        assert_eq!(named.len(), 1);
    }

    /// A file that cannot be read has to surface as a failure: reporting
    /// "clean" for a mistyped path is the one answer an agent must never get.
    #[test]
    fn check_all_flags_unreadable_files() {
        let s = testutil::scratch("scan-fail");
        let good = s.path("good.md");
        std::fs::write(&good, "teh cdoe\n").unwrap();
        let missing = s.path("nope.md");

        let engine = test_engine();
        let scan = check_all(
            &[good.clone(), missing],
            format::Format::Auto,
            &engine,
            false,
        )
        .unwrap();
        assert!(scan.failed, "unreadable file did not register as a failure");
        assert_eq!(scan.errors.len(), 1);
        assert!(
            !scan.misspellings.is_empty(),
            "the readable file should still have been checked"
        );

        // All readable: no failure.
        let scan = check_all(&[good], format::Format::Auto, &engine, false).unwrap();
        assert!(!scan.failed);
        assert!(scan.errors.is_empty());
    }
}
