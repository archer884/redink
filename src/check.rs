//! Driving a spellcheck pass over files into a list of [`Misspelling`] records.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::engine::Engine;
use crate::format::{self, Format};
use crate::token::tokenize;

/// A single misspelled-word occurrence in a file.
#[derive(Debug, Clone)]
pub struct Misspelling {
    pub path: PathBuf,
    /// 1-based line number.
    pub line: usize,
    /// 1-based byte column within the line.
    pub col: usize,
    /// Absolute byte offset of the word start in the file.
    pub byte_offset: usize,
    pub word: String,
    pub suggestions: Vec<String>,
}

/// Check a single file, returning its misspellings in source order.
pub fn check_file(
    path: &Path,
    format: Format,
    engine: &Engine,
    suggest_cache: &mut HashMap<String, Vec<String>>,
    suggest_limit: usize,
) -> Result<Vec<Misspelling>> {
    let src = std::fs::read_to_string(path)?;
    let line_starts = LineStarts::new(&src);
    let skip = format::skip_ranges(&src, format.resolve(path));
    let tokens = tokenize(&src, &skip);

    let mut out = Vec::new();
    for tok in tokens {
        if engine.check(&tok.word) {
            continue;
        }
        let (line0, col0) = line_starts.locate(tok.byte_range.start);
        let suggestions = suggest_cache
            .entry(tok.word.clone())
            .or_insert_with(|| engine.suggest(&tok.word))
            .iter()
            .take(suggest_limit)
            .cloned()
            .collect::<Vec<_>>();
        out.push(Misspelling {
            path: path.to_path_buf(),
            line: line0 + 1,
            col: col0 + 1,
            byte_offset: tok.byte_range.start,
            word: tok.word.clone(),
            suggestions,
        });
    }
    Ok(out)
}

/// Map an absolute byte offset to a 0-based (line, byte-column).
struct LineStarts {
    starts: Vec<usize>,
}

impl LineStarts {
    fn new(src: &str) -> Self {
        let mut starts = vec![0usize];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Self { starts }
    }

    fn locate(&self, byte_offset: usize) -> (usize, usize) {
        let line = self
            .starts
            .partition_point(|&s| s <= byte_offset)
            .saturating_sub(1);
        let col = byte_offset - self.starts[line];
        (line, col)
    }
}
