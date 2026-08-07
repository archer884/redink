//! Output formatters for check results: human-readable text, JSON (for agents),
//! and a plain list of unique misspelled words.

use std::collections::BTreeSet;
use std::io::Write;

use crate::check::Misspelling;

/// `file:line:col  word  ->  sug, sug` per occurrence.
pub fn write_text<W: Write>(out: &mut W, miss: &[Misspelling]) -> std::io::Result<()> {
    for m in miss {
        let sugs = m.suggestions.join(", ");
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

/// A JSON array, one object per occurrence (stable schema for agents).
pub fn write_json<W: Write>(out: &mut W, miss: &[Misspelling]) -> anyhow::Result<()> {
    let rows: Vec<serde_json::Value> = miss
        .iter()
        .map(|m| {
            serde_json::json!({
                "file": m.path.display().to_string(),
                "line": m.line,
                "column": m.col,
                "byte_offset": m.byte_offset,
                "word": m.word,
                "suggestions": m.suggestions,
                "compound": m.compound.as_ref().map(|(w, _)| w),
            })
        })
        .collect();
    let json = serde_json::to_string_pretty(&rows)?;
    out.write_all(json.as_bytes())?;
    writeln!(out)?;
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
    let word = if miss.len() == 1 { "misspelling" } else { "misspellings" };
    format!(
        "{} {word} across {} file{}",
        miss.len(),
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    )
}
