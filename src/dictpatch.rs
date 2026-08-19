//! Exact-line patching of Hunspell `.dic` dictionaries.
//!
//! The vendored dictionary stays pristine; local fixes live in a manifest
//! (`assets/dict/en_US.patches`, one `old -> new` line replacement per
//! record). [`apply_patches`] is run by `build.rs` at compile time to
//! produce the embedded copy — so a stale anchor (upstream changed or
//! already fixed) fails the build. The same file is compiled into build.rs
//! via `#[path]`, which means cargo does not run its tests there; the main
//! crate declares it as a test-only module so `cargo test` covers it.

/// Apply the manifest `patches` to the dictionary text `dic`, replacing the
/// first line exactly equal to each record's left-hand side. Comments (`#`)
/// and blank lines are ignored. On a malformed record or a missing anchor,
/// returns an error describing the offending manifest line.
pub fn apply_patches(dic: &str, patches: &str) -> Result<String, String> {
    const SEPARATOR: &str = " -> ";

    let ends_with_newline = dic.ends_with('\n');
    let mut lines: Vec<&str> = dic.lines().collect();

    for (lineno, record) in patches.lines().enumerate() {
        if record.is_empty() || record.starts_with('#') {
            continue;
        }
        let lineno = lineno + 1;
        let (from, to) = match record.split_once(SEPARATOR) {
            Some((from, to)) if !from.is_empty() && !to.is_empty() => (from, to),
            _ => {
                return Err(format!(
                    "line {lineno}: expected `old{SEPARATOR}new`, found {record:?} \
                     (additions/deletions unsupported; new words go in the working dictionary)"
                ));
            }
        };
        match lines.iter().position(|line| *line == from) {
            Some(idx) => lines[idx] = to,
            None => {
                return Err(format!(
                    "line {lineno}: anchor {from:?} not found — \
                     upstream changed or already fixed; update the manifest"
                ));
            }
        }
    }

    let mut out = lines.join("\n");
    if ends_with_newline {
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::apply_patches;

    const DIC: &str = "4\nelse\nelsewhere\nsaddler/S\n";

    #[test]
    fn applies_line_exact_replacements() {
        let out = apply_patches(DIC, "else -> else/M\nsaddler/S -> saddler/SM\n").unwrap();
        assert_eq!(out, "4\nelse/M\nelsewhere\nsaddler/SM\n");
    }

    #[test]
    fn substring_lines_are_not_touched() {
        let out = apply_patches(DIC, "else -> else/M").unwrap();
        assert!(
            out.contains("\nelsewhere\n"),
            "prefix line was modified: {out:?}"
        );
    }

    #[test]
    fn comments_and_blanks_are_ignored() {
        let out = apply_patches(DIC, "# comment\n\nelse -> else/M\n").unwrap();
        assert_eq!(out, "4\nelse/M\nelsewhere\nsaddler/S\n");
    }

    #[test]
    fn missing_anchor_is_an_error() {
        let err = apply_patches(DIC, "florp -> florp/S").unwrap_err();
        assert!(err.contains("anchor"), "{err}");
    }

    #[test]
    fn malformed_records_are_errors() {
        for bad in ["no-separator", " -> x", "x -> "] {
            assert!(apply_patches(DIC, bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn missing_trailing_newline_is_preserved() {
        let out = apply_patches("2\nelse\nx", "else -> else/M").unwrap();
        assert_eq!(out, "2\nelse/M\nx");
    }
}
