//! Output formatters for check results: human-readable text, JSON (for agents),
//! and a plain list of unique misspelled words.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::io::{BufWriter, Write};

use serde::{Serialize, Serializer};

use crate::check::Misspelling;

/// `file:line:col  word  ->  sug, sug` per occurrence.
pub fn write_text<W: Write>(out: &mut W, miss: &[Misspelling]) -> std::io::Result<()> {
    for m in miss {
        let sugs = m
            .suggestions
            .as_ref()
            .map(|s| s.join(", "))
            .unwrap_or_default();
        if sugs.is_empty() {
            writeln!(out, "{}:{}:{}  {}", m.path.display(), m.line, m.col, m.word)?;
        } else {
            writeln!(
                out,
                "{}:{}:{}  {}  ->  {}",
                m.path.display(),
                m.line,
                m.col,
                m.word,
                sugs
            )?;
        }
    }
    Ok(())
}

/// One occurrence as it appears in `check --json`.
///
/// A borrowing view over a [`Misspelling`] rather than a `Serialize` impl on
/// the type itself: these field names and their order are the published schema
/// that agents key off, so they are written down here once, where changing one
/// is a visible edit to the contract. The internal struct stays free to evolve.
#[derive(Serialize)]
struct Row<'a> {
    file: Cow<'a, str>,
    line: usize,
    column: usize,
    byte_offset: usize,
    word: &'a str,
    /// Always an array — never `null` — even before suggestions are computed.
    suggestions: &'a [String],
    /// The whole hyphenated token; its position is an internal detail of the
    /// editor, not part of the schema.
    compound: Option<&'a str>,
}

impl<'a> From<&'a Misspelling> for Row<'a> {
    fn from(m: &'a Misspelling) -> Self {
        Row {
            // Lossy like `Path::display`, but borrows for the UTF-8 paths that
            // are all anyone actually has.
            file: m.path.to_string_lossy(),
            line: m.line,
            column: m.col,
            byte_offset: m.byte_offset,
            word: &m.word,
            suggestions: m.suggestions.as_deref().unwrap_or(&[]),
            compound: m.compound.as_ref().map(|c| c.text.as_str()),
        }
    }
}

/// The whole report. `collect_seq` streams the rows into the writer instead of
/// materializing either a `serde_json::Value` tree or a `Vec<Row>` first.
struct Rows<'a>(&'a [Misspelling]);

impl Serialize for Rows<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(Row::from))
    }
}

/// A JSON array, one object per occurrence (stable schema for agents).
pub fn write_json<W: Write>(out: &mut W, miss: &[Misspelling]) -> anyhow::Result<()> {
    // Buffered: `to_writer_pretty` emits in small pieces and stdout is
    // line-buffered, so without this every line of a large report would be its
    // own write syscall.
    let mut out = BufWriter::new(out);
    serde_json::to_writer_pretty(&mut out, &Rows(miss))?;
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

/// Unique misspelled words, one per line, sorted (handy for piping).
pub fn write_words<W: Write>(out: &mut W, miss: &[Misspelling]) -> std::io::Result<()> {
    let unique: BTreeSet<&str> = miss.iter().map(|m| m.word.as_str()).collect();
    for w in unique {
        writeln!(out, "{w}")?;
    }
    Ok(())
}

/// A one-line summary for stderr, e.g. "3 misspellings in 2 files".
pub fn summary(miss: &[Misspelling]) -> String {
    let files: BTreeSet<&std::path::Path> = miss.iter().map(|m| m.path.as_path()).collect();
    let word = if miss.len() == 1 {
        "misspelling"
    } else {
        "misspellings"
    };
    format!(
        "{} {word} across {} file{}",
        miss.len(),
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{Compound, Misspelling};
    use std::path::PathBuf;

    fn miss(word: &str, compound: Option<Compound>, sugs: Option<Vec<String>>) -> Misspelling {
        Misspelling {
            path: PathBuf::from("ch01.md"),
            line: 42,
            col: 13,
            byte_offset: 1234,
            word: word.to_string(),
            suggestions: sugs,
            compound,
        }
    }

    /// The JSON shape is a published contract (README, AGENTS.md): agents key
    /// off these exact field names, and `compound` is the token text.
    #[test]
    fn json_schema_is_stable() {
        let rows = vec![
            miss("teh", None, Some(vec!["the".into(), "tech".into()])),
            miss(
                "teh",
                Some(Compound {
                    text: "teh-bar".into(),
                    byte_offset: 1234,
                }),
                None,
            ),
        ];
        let mut out = Vec::new();
        write_json(&mut out, &rows).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let first = &parsed[0];

        // Field order is part of what the docs show, and the derive fixes it.
        // Checked against the emitted text: parsing back into a `Value` sorts
        // the keys, so the parsed form cannot see the order at all.
        let text = String::from_utf8(out.clone()).unwrap();
        let order: Vec<usize> = [
            "\"file\"",
            "\"line\"",
            "\"column\"",
            "\"byte_offset\"",
            "\"word\"",
            "\"suggestions\"",
            "\"compound\"",
        ]
        .iter()
        .map(|k| {
            text.find(k)
                .unwrap_or_else(|| panic!("{k} missing: {text}"))
        })
        .collect();
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "fields are out of documented order: {text}"
        );

        assert_eq!(first["file"], "ch01.md");
        assert_eq!(first["line"], 42);
        assert_eq!(first["column"], 13);
        assert_eq!(first["byte_offset"], 1234);
        assert_eq!(first["word"], "teh");
        assert_eq!(first["suggestions"][0], "the");
        assert!(first["compound"].is_null());

        // A compound reports the whole token as a plain string, not an object.
        assert_eq!(parsed[1]["compound"], "teh-bar");
        // Missing suggestions render as an empty list, never as null.
        assert_eq!(
            parsed[1]["suggestions"],
            serde_json::json!(Vec::<String>::new())
        );
    }

    #[test]
    fn text_output_lists_occurrences_with_suggestions() {
        let rows = vec![
            miss("teh", None, Some(vec!["the".into(), "tech".into()])),
            miss("cdoe", None, None),
        ];
        let mut out = Vec::new();
        write_text(&mut out, &rows).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("ch01.md:42:13  teh  ->  the, tech"), "{text}");
        // No suggestions: no arrow, no trailing separator.
        assert!(text.contains("ch01.md:42:13  cdoe\n"), "{text}");
    }

    #[test]
    fn words_output_is_unique_and_sorted() {
        let rows = vec![
            miss("teh", None, None),
            miss("cdoe", None, None),
            miss("teh", None, None),
        ];
        let mut out = Vec::new();
        write_words(&mut out, &rows).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "cdoe\nteh\n");
    }

    #[test]
    fn summary_counts_occurrences_and_files() {
        let mut rows = vec![miss("teh", None, None), miss("cdoe", None, None)];
        assert_eq!(summary(&rows), "2 misspellings across 1 file");
        rows[1].path = PathBuf::from("ch02.md");
        assert_eq!(summary(&rows), "2 misspellings across 2 files");
        assert_eq!(summary(&rows[..1]), "1 misspelling across 1 file");
    }
}
