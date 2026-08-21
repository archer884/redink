//! Driving a spellcheck pass over files into a list of [`Misspelling`] records.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::engine::Engine;
use crate::format::{self, Format};
use crate::token::{Token, tokenize};

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
    pub suggestions: Option<Vec<String>>,
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
    needs_suggestions: bool,
) -> Result<Vec<Misspelling>> {
    let src = std::fs::read_to_string(path)?;
    let line_starts = LineStarts::new(&src);
    let skip = format::skip_ranges(&src, format.resolve(path));
    let tokens = tokenize(&src, &skip);
    let phrases = engine.phrase_bigrams();
    let phrases_cs = engine.phrase_bigrams_cs();

    let mut out = Vec::new();
    for (i, tok) in tokens.iter().enumerate() {
        // Phrase matching: a token that forms a known bigram with its neighbour
        // (e.g. "se" in "per se") is accepted, so fragment words are only let
        // through in their phrase context and still flagged elsewhere.
        if phrase_covered(i, &tokens, phrases, phrases_cs) {
            continue;
        }
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
                    needs_suggestions,
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
                needs_suggestions,
            ));
        }
    }
    Ok(out)
}

/// True if `tokens[i]` forms a known phrase bigram with either neighbour.
/// Regular phrases match case-insensitively; exact-case phrases (`=Tzeya
/// Gan`) require the neighbouring words to match as written.
pub(crate) fn phrase_covered(
    i: usize,
    tokens: &[Token],
    phrases: &HashSet<String>,
    phrases_cs: &HashSet<String>,
) -> bool {
    let w = tokens[i].word.to_lowercase();
    if i > 0 {
        let p = tokens[i - 1].word.to_lowercase();
        if phrases.contains(&format!("{p} {w}")) {
            return true;
        }
    }
    if i + 1 < tokens.len() {
        let n = tokens[i + 1].word.to_lowercase();
        if phrases.contains(&format!("{w} {n}")) {
            return true;
        }
    }
    if i > 0 && phrases_cs.contains(&format!("{} {}", tokens[i - 1].word, tokens[i].word)) {
        return true;
    }
    if i + 1 < tokens.len()
        && phrases_cs.contains(&format!("{} {}", tokens[i].word, tokens[i + 1].word))
    {
        return true;
    }
    false
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
    needs_suggestions: bool,
) -> Misspelling {
    let (line0, col0) = line_starts.locate(byte_range.start);
    let suggestions = if needs_suggestions {
        Some(
            suggest_cache
                .entry(word.to_string())
                .or_insert_with(|| engine.suggest(word))
                .iter()
                .take(suggest_limit)
                .cloned()
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
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
            vec![("Seneca", 10..16), ("by", 17..19), ("Stone", 20..25),]
        );
    }

    fn check_str(src: &str) -> Vec<String> {
        // Write to a temp file and run a real check with the bundled engine.
        // A process-unique atomic counter avoids collisions between parallel
        // tests (subsec_nanos alone can repeat under load).
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("redink-check-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.md");
        std::fs::write(&path, src).unwrap();

        let sys = crate::sysdict::resolve_embedded();
        let dict = crate::engine::load_dictionary(&sys.aff, &sys.dic).unwrap();
        let engine = crate::engine::Engine::new(
            dict,
            crate::dict::WorkingDict::default(),
            std::path::PathBuf::from("/dev/null"),
        );
        let mut cache = HashMap::new();
        let miss = check_file(&path, Format::Auto, &engine, &mut cache, 9, true).unwrap();
        miss.into_iter().map(|m| m.word).collect()
    }

    fn check_str_with(src: &str, working: crate::dict::WorkingDict) -> Vec<String> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("redink-checkcs-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.md");
        std::fs::write(&path, src).unwrap();

        let sys = crate::sysdict::resolve_embedded();
        let dict = crate::engine::load_dictionary(&sys.aff, &sys.dic).unwrap();
        let engine =
            crate::engine::Engine::new(dict, working, std::path::PathBuf::from("/dev/null"));
        let mut cache = HashMap::new();
        let miss = check_file(&path, Format::Auto, &engine, &mut cache, 9, true).unwrap();
        miss.into_iter().map(|m| m.word).collect()
    }

    #[test]
    fn phrase_matching() {
        // "se" is accepted inside "per se" but flagged on its own.
        let m = check_str("It was, per se, fine. But se alone is a typo.");
        assert!(
            !m.contains(&"se".to_string()) || m.iter().filter(|w| *w == "se").count() == 1,
            "expected exactly one 'se' (the standalone): {m:?}"
        );
        assert!(m.contains(&"se".to_string()));
    }

    #[test]
    fn roman_numerals_and_acronyms_skipped() {
        let m = check_str("Chapter XIV bore MK17 rifles and XVII flags.");
        assert!(
            !m.iter().any(|w| w.chars().all(|c| c.is_ascii_uppercase())),
            "all-caps token flagged: {m:?}"
        );
    }

    /// Scene-label fractions like "2/1's" tokenize to numeric possessives
    /// ("1's"), and lone foreign letters ("Θ") are notation — neither is a
    /// misspelling.
    #[test]
    fn numeric_possessives_and_single_letters_not_flagged() {
        let m = check_str("The relocation 2/1's pacing; 2/5's architecture; the rune Θ held.");
        assert!(!m.contains(&"1's".to_string()), "1's flagged: {m:?}");
        assert!(!m.contains(&"5's".to_string()), "5's flagged: {m:?}");
        assert!(!m.contains(&"Θ".to_string()), "Θ flagged: {m:?}");
    }

    #[test]
    fn latin_phrases_accepted() {
        let m = check_str("A de facto rule; in vitro; ad hoc; ipso facto; bona fide.");
        // none of the Latin fragment words should be flagged
        for bad in ["facto", "vitro", "ipso", "bona", "fide"] {
            assert!(!m.contains(&bad.to_string()), "{bad} flagged: {m:?}");
        }
    }

    /// An exact-case phrase accepts its fragment words only when the
    /// neighbouring text matches the phrase as written.
    #[test]
    fn case_sensitive_phrase_exact_match_only() {
        let mut wd = crate::dict::WorkingDict::default();
        wd.add_entry("Tzeya Gan", true);
        let m = check_str_with("The Tzeya Gan rose. The tzeya gan fell.", wd);
        assert!(
            !m.contains(&"Tzeya".to_string()),
            "exact phrase flagged: {m:?}"
        );
        assert!(
            !m.contains(&"Gan".to_string()),
            "exact phrase flagged: {m:?}"
        );
        assert_eq!(
            m,
            vec!["tzeya".to_string(), "gan".to_string()],
            "wrong-case phrase must still be flagged: {m:?}"
        );
    }

    /// A case-insensitive phrase still accepts any casing (the cs layer must
    /// not narrow existing behaviour).
    #[test]
    fn case_insensitive_phrase_still_fuzzy() {
        let mut wd = crate::dict::WorkingDict::default();
        wd.add_entry("tzeya gan", false);
        let m = check_str_with("The tzeya gan rose. The TZeya GAN fell.", wd);
        assert!(m.is_empty(), "ci phrase should accept any casing: {m:?}");
    }
}
