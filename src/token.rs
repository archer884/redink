//! Word tokenization with byte-accurate source ranges.
//!
//! A "word" is a maximal run of characters that are alphanumeric, ASCII
//! apostrophe (`'`), or the right single quote (`’` U+2019) — matching the
//! WORDCHARS convention used by the bundled dictionary. Tokens are reported
//! with their absolute byte range in the source so that replacements and
//! locations are always exact. Tokens falling inside any "skip" range (code,
//! URLs, frontmatter) are dropped, as are tokens with no alphabetic character.

use std::ops::Range;

#[derive(Debug, Clone)]
pub struct Token {
    pub word: String,
    pub byte_range: Range<usize>,
}

/// Tokens plus a parallel array of their lowercased words, so phrase
/// matching never re-lowercases per lookup.
#[derive(Debug, Clone)]
pub struct Tokenized {
    pub tokens: Vec<Token>,
    pub lowercase: Vec<String>,
}

#[inline]
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\'' || c == '\u{2019}' || c == '-'
}

/// Split `src` into word tokens, excluding any whose byte range overlaps a
/// range in `skip` and any token containing no alphabetic character.
pub fn tokenize(src: &str, skip: &[Range<usize>]) -> Vec<Token> {
    let skip = merge_ranges(skip);
    let mut out = Vec::new();
    let mut start: Option<usize> = None;

    for (idx, c) in src.char_indices() {
        if is_word_char(c) {
            if start.is_none() {
                start = Some(idx);
            }
        } else if let Some(s) = start.take() {
            flush(src, s, idx, &skip, &mut out);
        }
    }
    if let Some(s) = start.take() {
        flush(src, s, src.len(), &skip, &mut out);
    }
    out
}

/// [`tokenize`] with the lowercase companion array for phrase matching.
pub fn tokenize_with_lowercase(src: &str, skip: &[Range<usize>]) -> Tokenized {
    let tokens = tokenize(src, skip);
    let lowercase = tokens.iter().map(|t| t.word.to_lowercase()).collect();
    Tokenized { tokens, lowercase }
}

/// Sort and merge the skip ranges into a disjoint, ordered set, so an
/// overlap check per token is a single binary search.
fn merge_ranges(ranges: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut sorted: Vec<Range<usize>> = ranges.to_vec();
    sorted.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(sorted.len());
    for r in sorted {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

/// True if `range` overlaps any range in the merged, sorted `skip` set.
fn range_skipped(skip: &[Range<usize>], range: &Range<usize>) -> bool {
    let idx = skip.partition_point(|r| r.end <= range.start);
    skip.get(idx).is_some_and(|r| r.start < range.end)
}

fn flush(src: &str, s: usize, e: usize, skip: &[Range<usize>], out: &mut Vec<Token>) {
    let bytes = src.as_bytes();
    let mut ws = s;
    let mut we = e;
    // Trim surrounding ASCII apostrophes and hyphens (e.g. 'hello', foo-, -bar)
    // but keep interior ones and the Unicode right quote, which the dictionary
    // treats as a word char. Hyphens join compound tokens (Tzeya-Gan); leading
    // and trailing hyphens (line-break markers, `--` em-dashes at edges) drop.
    while ws < we && (bytes[ws] == b'\'' || bytes[ws] == b'-') {
        ws += 1;
    }
    while we > ws && (bytes[we - 1] == b'\'' || bytes[we - 1] == b'-') {
        we -= 1;
    }
    if ws >= we {
        return;
    }
    let range = ws..we;
    if range_skipped(skip, &range) {
        return;
    }
    let word = &src[range.clone()];
    if !word.chars().any(|c| c.is_alphabetic()) {
        return;
    }
    out.push(Token {
        word: word.to_string(),
        byte_range: range,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_words() {
        let t = tokenize("Hello, world! Don't 123.", &[]);
        let words: Vec<&str> = t.iter().map(|x| x.word.as_str()).collect();
        assert_eq!(words, vec!["Hello", "world", "Don't"]);
    }

    #[test]
    fn skips_ranges() {
        let src = "keep `drop1` keep drop2";
        let skip = vec![6..13]; // covers `drop1`
        let t = tokenize(src, &skip);
        let words: Vec<&str> = t.iter().map(|x| x.word.as_str()).collect();
        assert_eq!(words, vec!["keep", "keep", "drop2"]);
    }

    #[test]
    fn skips_ranges_merge_unordered_and_overlapping() {
        let src = "a drop1 b drop2 c"; // drop1 = 2..7, drop2 = 10..15
        let skip = vec![10..15, 5..5, 2..7];
        let t = tokenize(src, &skip);
        let words: Vec<&str> = t.iter().map(|x| x.word.as_str()).collect();
        assert_eq!(words, vec!["a", "b", "c"]);
    }

    #[test]
    fn smart_quote_apostrophe() {
        let t = tokenize("can’t", &[]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].word, "can’t");
    }

    #[test]
    fn hyphenated_compound_is_one_token() {
        let t = tokenize("Tzeya-Gan rode forth", &[]);
        let words: Vec<&str> = t.iter().map(|x| x.word.as_str()).collect();
        assert_eq!(words, vec!["Tzeya-Gan", "rode", "forth"]);
        // byte range covers the whole compound including the hyphen
        assert_eq!(t[0].byte_range, 0..9);
    }

    #[test]
    fn leading_trailing_hyphens_trimmed() {
        // "--Athrune" (em-dash before a name) collapses to "Athrune"
        let t = tokenize("year--another", &[]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].word, "year--another");
        let t2 = tokenize("Athrune--", &[]);
        assert_eq!(t2[0].word, "Athrune");
        let t3 = tokenize("-foo-", &[]);
        assert_eq!(t3[0].word, "foo");
    }
}
