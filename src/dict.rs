//! The working (project) dictionary: a plain-text, git-friendly wordlist that
//! augments the system dictionary. Additions from the TUI/CLI are written here
//! and only here — the system dictionary is never modified.
//!
//! File format (UTF-8, one entry per line):
//! ```text
//! # redink working dictionary
//! # bare word      -> case-insensitive (accepted in any case; stored lowercase)
//! # =Word          -> case-sensitive   (accepted only in that exact casing)
//! hobbit
//! elflord
//! =Gondor
//! ```
//! Lines starting with `#` are comments; blank lines are ignored.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

const DEFAULT_NAME: &str = ".redink.dic";
const HEADER: &[&str] = &[
    "# redink working dictionary",
    "# bare word: case-insensitive  |  =Word: case-sensitive (exact case)",
];

#[derive(Debug, Default, Clone)]
pub struct WorkingDict {
    /// Case-insensitive entries, stored lowercase.
    pub ci: HashSet<String>,
    /// Case-sensitive entries, stored in exact casing.
    pub cs: HashSet<String>,
}

/// Strip a trailing English possessive (`'s` / `'s` / `'S` / `'S`, using an
/// ASCII apostrophe or the Unicode right quote U+2019). Returns the stem, or
/// `None` if `word` has no possessive ending (so plurals like "dogs" and
/// contractions like "can't" are left alone).
pub fn strip_possessive(word: &str) -> Option<&str> {
    let mut chars = word.chars();
    let last = chars.next_back()?;
    if last != 's' && last != 'S' {
        return None;
    }
    let penult = chars.next_back()?;
    if penult != '\'' && penult != '\u{2019}' {
        return None;
    }
    let new_len = word.len() - penult.len_utf8() - last.len_utf8();
    if new_len == 0 {
        return None;
    }
    Some(&word[..new_len])
}

/// Canonical form stored in the working dictionary: the possessive stem. So
/// adding "Atrax's" registers "Atrax", and (via the engine's check-time
/// stripping) "Atrax's" is accepted too. Words without a possessive ending are
/// returned unchanged.
pub fn canonical(word: &str) -> String {
    strip_possessive(word).unwrap_or(word).to_string()
}

impl WorkingDict {
    pub fn add_ci(&mut self, word: &str) -> bool {
        self.ci.insert(canonical(word).to_lowercase())
    }

    pub fn add_cs(&mut self, word: &str) -> bool {
        self.cs.insert(canonical(word))
    }

    pub fn remove(&mut self, word: &str) -> bool {
        let c = canonical(word);
        self.ci.remove(&c.to_lowercase()) || self.cs.remove(&c)
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.ci.is_empty() && self.cs.is_empty()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.ci.len() + self.cs.len()
    }
}

/// Locate the working dictionary.
///
/// With no override, searches from the current directory upward for
/// `DEFAULT_NAME` (so it can live at a repo root). If none exists, returns
/// `<cwd>/DEFAULT_NAME`, which will be created on first save.
pub fn locate(override_path: Option<&Path>) -> PathBuf {
    if let Some(p) = override_path {
        return p.to_path_buf();
    }
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    loop {
        let candidate = dir.join(DEFAULT_NAME);
        if candidate.is_file() {
            return candidate;
        }
        if !dir.pop() {
            break;
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(DEFAULT_NAME)
}

/// Load the working dictionary at `path` if it exists, else an empty one.
pub fn load(path: &Path) -> anyhow::Result<WorkingDict> {
    let mut dict = WorkingDict::default();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(dict),
        Err(e) => return Err(e.into()),
    };
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('=') {
            if !rest.is_empty() {
                dict.add_cs(rest);
            }
        } else {
            dict.add_ci(line);
        }
    }
    Ok(dict)
}

/// Persist the working dictionary, sorted for clean diffs.
pub fn save(path: &Path, dict: &WorkingDict) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut ci: Vec<&String> = dict.ci.iter().collect();
    ci.sort();
    let mut cs: Vec<&String> = dict.cs.iter().collect();
    cs.sort();

    let mut out = String::new();
    for h in HEADER {
        out.push_str(h);
        out.push('\n');
    }
    for w in &ci {
        out.push_str(w);
        out.push('\n');
    }
    for w in &cs {
        out.push('=');
        out.push_str(w);
        out.push('\n');
    }
    std::fs::write(path, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!("redink-test-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn roundtrip() {
        let p = scratch("d.dic");
        let mut d = WorkingDict::default();
        d.add_ci("hobbit");
        d.add_cs("Gondor");
        save(&p, &d).unwrap();

        let loaded = load(&p).unwrap();
        assert!(loaded.ci.contains("hobbit"));
        assert!(loaded.cs.contains("Gondor"));
        assert!(!loaded.ci.contains("gondor"));
    }

    #[test]
    fn missing_file_is_empty() {
        let p = scratch("nope.dic");
        assert!(load(&p).unwrap().is_empty());
    }

    #[test]
    fn strip_possessive_cases() {
        assert_eq!(strip_possessive("Thorne's"), Some("Thorne"));
        assert_eq!(strip_possessive("Thorne\u{2019}s"), Some("Thorne"));
        assert_eq!(strip_possessive("THE'S"), Some("THE"));
        assert_eq!(strip_possessive("dogs"), None);
        assert_eq!(strip_possessive("can't"), None);
        assert_eq!(strip_possessive("'s"), None);
        assert_eq!(strip_possessive("a"), None);
        // plural possessive (s') is NOT stripped
        assert_eq!(strip_possessive("dogs'"), None);
    }

    #[test]
    fn add_canonicalizes_possessive() {
        let mut d = WorkingDict::default();
        // adding the possessive form stores the stem
        d.add_ci("Atrax's");
        assert!(d.ci.contains("atrax"));
        assert!(!d.ci.contains("atrax's"));
        d.add_cs("Tzeya-Gan\u{2019}s");
        assert!(d.cs.contains("Tzeya-Gan"));
        // a plain word is unchanged
        d.add_ci("hobbit");
        assert!(d.ci.contains("hobbit"));
    }

    #[test]
    fn remove_canonicalizes_possessive() {
        let mut d = WorkingDict::default();
        d.add_ci("atrax");
        assert!(d.remove("Atrax's")); // remove via the possessive form
        assert!(!d.ci.contains("atrax"));
    }
}
