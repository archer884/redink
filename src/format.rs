//! Input-format handling. Each format produces the set of byte ranges that
//! should *not* be spellchecked (code, URLs, frontmatter). Word tokenization
//! itself is format-agnostic and lives in [`token`].

use std::ops::Range;

use clap::ValueEnum;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Detect from the file extension (markdown for `.md`/`.markdown`, else text).
    Auto,
    /// Markdown: skip fenced/indented code blocks, inline code, URLs, and
    /// YAML/TOML frontmatter. `md` is accepted too — nobody wants to type the
    /// long form at a prompt.
    #[value(alias = "md")]
    Markdown,
    /// Plain text: check everything.
    Text,
}

/// Extensions treated as Markdown when discovering files and resolving
/// `Format::Auto`.
const MARKDOWN_EXTS: &[&str] = &["md", "markdown", "mdown", "mkd"];
/// Additional plain-text extensions discovered by file walks.
const TEXT_EXTS: &[&str] = &["txt", "text"];

fn lower_ext(path: &std::path::Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

impl Format {
    /// Resolve [`Format::Auto`] against a file extension.
    pub fn resolve(self, path: &std::path::Path) -> Self {
        match self {
            Format::Auto => {
                let ext = lower_ext(path);
                if MARKDOWN_EXTS.contains(&ext.as_str()) {
                    Format::Markdown
                } else {
                    Format::Text
                }
            }
            other => other,
        }
    }

    /// True when `path` has an extension worth spellchecking (Markdown or
    /// plain text).
    pub fn is_checkable(path: &std::path::Path) -> bool {
        let ext = lower_ext(path);
        MARKDOWN_EXTS.contains(&ext.as_str()) || TEXT_EXTS.contains(&ext.as_str())
    }
}

/// Byte ranges in `src` that should be excluded from spellchecking.
pub fn skip_ranges(src: &str, format: Format) -> Vec<Range<usize>> {
    match format {
        Format::Markdown => markdown_skip(src),
        Format::Auto | Format::Text => Vec::new(),
    }
}

fn markdown_skip(src: &str) -> Vec<Range<usize>> {
    let mut skips = Vec::new();

    if let Some(r) = frontmatter_range(src) {
        skips.push(r);
    }

    let mut code_depth = 0usize;
    for (event, range) in Parser::new(src).into_offset_iter() {
        match event {
            Event::Code(_) => skips.push(range),
            Event::Html(_) | Event::InlineHtml(_) => skips.push(range),
            Event::Start(Tag::CodeBlock(_)) => code_depth += 1,
            Event::End(TagEnd::CodeBlock) => code_depth = code_depth.saturating_sub(1),
            Event::Text(_) if code_depth > 0 => skips.push(range),
            _ => {}
        }
    }

    skips.extend(url_ranges(src));
    skips
}

/// Detect a leading YAML/TOML frontmatter block delimited by `---` lines.
/// Opening and closing fences may carry a trailing `\r` (CRLF files).
fn frontmatter_range(src: &str) -> Option<Range<usize>> {
    let bytes = src.as_bytes();
    let first_nl = bytes
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(bytes.len());
    if trim_cr(&bytes[..first_nl]) != b"---" {
        return None;
    }

    let mut search = first_nl + 1;
    while search < src.len() {
        let rest = &src[search..];
        let line_end = rest.find('\n').map(|p| search + p);
        let line = match line_end {
            Some(e) => &src[search..e],
            None => &src[search..],
        };
        let trimmed = trim_cr(line.as_bytes());
        if trimmed == b"---" || trimmed == b"..." {
            let end = line_end.map(|e| e + 1).unwrap_or(src.len());
            return Some(0..end);
        }
        match line_end {
            Some(e) => search = e + 1,
            None => break,
        }
    }
    None
}

/// A byte slice without one trailing ASCII `\r`, if present.
fn trim_cr(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

/// Bare-URL spans (`http(s)://…`, `ftp://…`) running to the next whitespace.
fn url_ranges(src: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    for scheme in ["http://", "https://", "ftp://"] {
        let mut from = 0;
        while let Some(rel) = src[from..].find(scheme) {
            let start = from + rel;
            let end = src[start..]
                .find(char::is_whitespace)
                .map(|p| start + p)
                .unwrap_or(src.len());
            out.push(start..end);
            from = end.max(start + scheme.len());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md_is_accepted_for_markdown() {
        use clap::ValueEnum;
        assert_eq!(Format::from_str("md", false).unwrap(), Format::Markdown);
        assert_eq!(
            Format::from_str("markdown", false).unwrap(),
            Format::Markdown
        );
        assert_eq!(Format::from_str("auto", false).unwrap(), Format::Auto);
        assert_eq!(Format::from_str("text", false).unwrap(), Format::Text);
        assert!(Format::from_str("bogus", false).is_err());
    }

    #[test]
    fn skips_inline_and_fenced_code() {
        let src = "this `cdoe` is fine\n\n```\ncdoe block\n```\nmore cdoe";
        let skips = markdown_skip(src);
        // The word "cdoe" appears 3 times: inline, fenced, and trailing.
        // Only the trailing one should remain after skipping.
        let tokens = crate::token::tokenize(src, &skips);
        let cdoes: Vec<_> = tokens
            .iter()
            .filter(|t| t.word == "cdoe")
            .map(|t| t.byte_range.start)
            .collect();
        assert_eq!(cdoes.len(), 1);
        assert!(cdoes[0] > src.find("more").unwrap());
    }

    #[test]
    fn frontmatter_detection() {
        let src = "---\ntitle: Hi\n---\nbody word";
        let r = frontmatter_range(src).unwrap();
        assert_eq!(r, 0..src.find("body").unwrap());
    }

    #[test]
    fn frontmatter_detection_crlf() {
        let src = "---\r\ntitle: Hi\r\n---\r\nbody word";
        let r = frontmatter_range(src).unwrap();
        assert_eq!(r, 0..src.find("body").unwrap());
        let skips = markdown_skip(src);
        let toks = crate::token::tokenize(src, &skips);
        let words: Vec<&str> = toks.iter().map(|t| t.word.as_str()).collect();
        assert!(words.contains(&"body"));
        assert!(
            !words.contains(&"title"),
            "CRLF frontmatter leaked: {words:?}"
        );
    }

    #[test]
    fn html_comment_is_skipped() {
        let src = "intro text\n\n<!-- TODO hidden ZZZXYZ -->\n\noutro text";
        let skips = markdown_skip(src);
        let toks = crate::token::tokenize(src, &skips);
        let words: Vec<&str> = toks.iter().map(|t| t.word.as_str()).collect();
        assert!(words.contains(&"intro"));
        assert!(words.contains(&"outro"));
        assert!(
            !words.contains(&"ZZZXYZ"),
            "comment contents leaked into tokens: {words:?}"
        );
        assert!(
            !words.contains(&"hidden"),
            "comment word 'hidden' leaked: {words:?}"
        );
    }
}
