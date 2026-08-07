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

#[inline]
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\'' || c == '\u{2019}' || c == '-'
}

/// Split `src` into word tokens, excluding any whose byte range overlaps a
/// range in `skip` and any token containing no alphabetic character.
pub fn tokenize(src: &str, skip: &[Range<usize>]) -> Vec<Token> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;

    for (idx, c) in src.char_indices() {
        if is_word_char(c) {
            if start.is_none() {
                start = Some(idx);
            }
        } else if let Some(s) = start.take() {
            flush(src, s, idx, skip, &mut out);
        }
    }
    if let Some(s) = start.take() {
        flush(src, s, src.len(), skip, &mut out);
    }
    out
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
    if skip.iter().any(|r| range.start < r.end && range.end > r.start) {
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
