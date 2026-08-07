//! The spellchecking engine: combines the (read-only) system dictionary with
//! the project working dictionary and an ephemeral session-ignore list.
//!
//! Checking precedence (first match wins):
//! 1. session-ignore (`i` in the TUI) — case-insensitive, never persisted
//! 2. working-dict case-sensitive entries (`A`) — exact casing only
//! 3. working-dict case-insensitive entries (`a`) — any casing
//! 4. the system dictionary

use std::collections::HashSet;
use std::path::PathBuf;

use spellbook::Dictionary;

use crate::dict::WorkingDict;

pub struct Engine {
    dict: Dictionary,
    working_path: PathBuf,
    working_ci: HashSet<String>,
    working_cs: HashSet<String>,
    session_ignore: HashSet<String>,
}

impl Engine {
    pub fn new(dict: Dictionary, working: WorkingDict, working_path: PathBuf) -> Self {
        Self {
            dict,
            working_path,
            working_ci: working.ci,
            working_cs: working.cs,
            session_ignore: HashSet::new(),
        }
    }

    pub fn working_path(&self) -> &PathBuf {
        &self.working_path
    }

    pub fn working_ci_count(&self) -> usize {
        self.working_ci.len()
    }

    pub fn working_cs_count(&self) -> usize {
        self.working_cs.len()
    }

    /// True if `word` is acceptable according to any dictionary layer.
    ///
    /// The session and working-dictionary layers also honor possessive-stripping
    /// so that adding a coinage (e.g. "Thorne") also accepts its possessive
    /// ("Thorne's"), since the working dictionary is a plain set outside
    /// spellbook's `'s` affix machinery.
    pub fn check(&self, word: &str) -> bool {
        if self.user_layer_has(word) {
            return true;
        }
        if strip_possessive(word).is_some_and(|s| self.user_layer_has(s)) {
            return true;
        }
        self.dict.check(word)
    }

    /// Session-ignore + working-dictionary (ci and cs) layers, case-sensitively
    /// correct. Does not consult the system dictionary.
    fn user_layer_has(&self, word: &str) -> bool {
        if self.session_ignore.contains(&word.to_lowercase()) {
            return true;
        }
        if self.working_cs.contains(word) {
            return true;
        }
        if self.working_ci.contains(&word.to_lowercase()) {
            return true;
        }
        false
    }

    /// Suggested corrections for `word` from the system dictionary.
    pub fn suggest(&self, word: &str) -> Vec<String> {
        let mut out = Vec::new();
        self.dict.checker().into_suggester().suggest(word, &mut out);
        out
    }

    /// Ignore `word` for the rest of this session only (case-insensitive).
    pub fn ignore_session(&mut self, word: &str) {
        self.session_ignore.insert(word.to_lowercase());
    }

    /// Add a case-insensitive entry (accepted in any casing). Persists on save.
    pub fn add_ci(&mut self, word: &str) {
        self.working_ci.insert(word.to_lowercase());
    }

    /// Add a case-sensitive entry (exact casing only). Persists on save.
    pub fn add_cs(&mut self, word: &str) {
        self.working_cs.insert(word.to_string());
    }

    /// Remove an entry (matches either layer). Persists on save.
    #[allow(dead_code)]
    pub fn remove(&mut self, word: &str) -> bool {
        let a = self.working_ci.remove(&word.to_lowercase());
        let b = self.working_cs.remove(word);
        a || b
    }

    /// Write the working dictionary back to disk.
    pub fn save_working(&self) -> anyhow::Result<()> {
        let dict = WorkingDict {
            ci: self.working_ci.clone(),
            cs: self.working_cs.clone(),
        };
        crate::dict::save(&self.working_path, &dict)
    }
}

/// Parse raw `.aff`/`.dic` text into a [`Dictionary`], mapping spellbook's
/// non-`std::error::Error` parse type into an `anyhow::Error`.
pub fn load_dictionary(aff: &str, dic: &str) -> anyhow::Result<Dictionary> {
    Dictionary::new(aff, dic).map_err(|e| anyhow::anyhow!("failed to parse dictionary: {e:?}"))
}

/// Strip a trailing English possessive (`'s` / `'s` / `'S` / `'S`, using an
/// ASCII apostrophe or the Unicode right quote U+2019). Returns the stem, or
/// `None` if `word` has no possessive ending (plurals and other contractions
/// like "dogs" / "can't" are left alone).
fn strip_possessive(word: &str) -> Option<&str> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::WorkingDict;
    use std::path::PathBuf;

    #[test]
    fn strip_possessive_cases() {
        assert_eq!(strip_possessive("Thorne's"), Some("Thorne"));
        assert_eq!(strip_possessive("Thorne\u{2019}s"), Some("Thorne"));
        assert_eq!(strip_possessive("THE'S"), Some("THE"));
        assert_eq!(strip_possessive("dogs"), None);
        assert_eq!(strip_possessive("can't"), None);
        assert_eq!(strip_possessive("'s"), None);
        assert_eq!(strip_possessive("a"), None);
    }

    fn engine_with_ci(word: &str) -> Engine {
        let sys = crate::sysdict::resolve("en_US", None).unwrap();
        let dict = load_dictionary(&sys.aff, &sys.dic).unwrap();
        let mut wd = WorkingDict::default();
        wd.add_ci(word);
        Engine::new(dict, wd, PathBuf::from("/dev/null"))
    }

    #[test]
    fn possessive_accepted_when_base_added() {
        let e = engine_with_ci("thorne");
        assert!(e.check("Thorne"));
        assert!(e.check("thorne"));
        assert!(e.check("Thorne's")); // ASCII possessive
        assert!(e.check("Thorne\u{2019}s")); // smart-quote possessive
        assert!(!e.check("Thornex"));
    }
}
