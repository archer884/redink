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
    /// If this occurrence is a part of a hyphenated compound that was not
    /// recognized as a whole, the full token text and its byte range. `None`
    /// for plain (non-compound) words.
    pub compound: Option<(String, std::ops::Range<usize>)>,
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
        if tok.word.contains('-') {
            // Stage 1: the whole compound. If any layer recognizes it, accept.
            if engine.check(&tok.word) {
                continue;
            }
            // Stage 2: split on hyphen-runs and check each non-empty part.
            let compound = (tok.word.clone(), tok.byte_range.clone());
            for (part, range) in split_hyphen_parts(&tok.word, tok.byte_range.start) {
                if part.is_empty() || !part.chars().any(|c| c.is_alphabetic()) {
                    continue;
                }
                if engine.check(part) {
                    continue;
                }
                out.push(build_misspelling(
                    path,
                    &line_starts,
                    part,
                    &range,
                    Some(&compound),
                    engine,
                    suggest_cache,
                    suggest_limit,
                ));
            }
        } else {
            if engine.check(&tok.word) {
                continue;
            }
            out.push(build_misspelling(
                path,
                &line_starts,
                &tok.word,
                &tok.byte_range,
                None,
                engine,
                suggest_cache,
                suggest_limit,
            ));
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn build_misspelling(
    path: &Path,
    line_starts: &LineStarts,
    word: &str,
    byte_range: &std::ops::Range<usize>,
    compound: Option<&(String, std::ops::Range<usize>)>,
    engine: &Engine,
    suggest_cache: &mut HashMap<String, Vec<String>>,
    suggest_limit: usize,
) -> Misspelling {
    let (line0, col0) = line_starts.locate(byte_range.start);
    let suggestions = suggest_cache
        .entry(word.to_string())
        .or_insert_with(|| engine.suggest(word))
        .iter()
        .take(suggest_limit)
        .cloned()
        .collect::<Vec<_>>();
    Misspelling {
        path: path.to_path_buf(),
        line: line0 + 1,
        col: col0 + 1,
        byte_offset: byte_range.start,
        word: word.to_string(),
        suggestions,
        compound: compound.map(|(w, r)| (w.clone(), r.clone())),
    }
}

/// Split `word` on hyphens into `(part, absolute_byte_range)` pairs, skipping
/// empty parts (so `year--another` yields only `year` and `another`). `base` is
/// the absolute byte offset at which `word` begins in the source.
fn split_hyphen_parts(word: &str, base: usize) -> Vec<(&str, std::ops::Range<usize>)> {
    let mut out = Vec::new();
    let bytes = word.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i <= bytes.len() {
        if i == bytes.len() || bytes[i] == b'-' {
            if i > start {
                out.push((&word[start..i], base + start..base + i));
            }
            start = i + 1;
        }
        i += 1;
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_parts_basic() {
        let parts = split_hyphen_parts("Tzeya-Gan", 100);
        assert_eq!(parts, vec![("Tzeya", 100..105), ("Gan", 106..109)]);
    }

    #[test]
    fn split_parts_double_hyphen_drops_empty() {
        let parts = split_hyphen_parts("year--another", 0);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], ("year", 0..4));
        assert_eq!(parts[1], ("another", 6..13));
    }

    #[test]
    fn split_parts_multi() {
        let parts = split_hyphen_parts("Seneca-by-Stone", 10);
        assert_eq!(
            parts,
            vec![
                ("Seneca", 10..16),
                ("by", 17..19),
                ("Stone", 20..25),
            ]
        );
    }
}
