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

use crate::dict::{canonical, strip_possessive, WorkingDict};

/// Minimum character length for a suggestion to be shown. Shorter ones are
/// almost always noise (e.g. "e", "s", "es").
const MIN_SUGGEST_LEN: usize = 3;

pub struct Engine {
    dict: Dictionary,
    working_path: PathBuf,
    working_ci: HashSet<String>,
    working_cs: HashSet<String>,
    working_phrases: HashSet<String>,
    session_ignore: HashSet<String>,
    phrase_bigrams: HashSet<String>,
}

impl Engine {
    pub fn new(dict: Dictionary, working: WorkingDict, working_path: PathBuf) -> Self {
        let phrase_bigrams = crate::dict::build_phrase_bigrams(&working.phrases);
        Self {
            dict,
            working_path,
            working_ci: working.ci,
            working_cs: working.cs,
            working_phrases: working.phrases,
            session_ignore: HashSet::new(),
            phrase_bigrams,
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

    /// Suggested corrections for `word` from the system dictionary. Very short
    /// suggestions (< 3 characters) are dropped — they're mostly noise.
    pub fn suggest(&self, word: &str) -> Vec<String> {
        let mut out = Vec::new();
        self.dict.checker().into_suggester().suggest(word, &mut out);
        out.retain(|s| s.chars().count() >= MIN_SUGGEST_LEN);
        out
    }

    /// Ignore `word` for the rest of this session only (case-insensitive).
    /// The stem is stored so that "Atrax's" also suppresses "Atrax".
    pub fn ignore_session(&mut self, word: &str) {
        self.session_ignore.insert(canonical(word).to_lowercase());
    }

    /// Add a case-insensitive entry (accepted in any casing). The possessive
    /// stem is stored, so adding "Atrax's" registers "Atrax". Persists on save.
    pub fn add_ci(&mut self, word: &str) {
        self.working_ci.insert(canonical(word).to_lowercase());
    }

    /// Add a case-sensitive entry (exact casing only). The possessive stem is
    /// stored. Persists on save.
    pub fn add_cs(&mut self, word: &str) {
        self.working_cs.insert(canonical(word));
    }

    /// Remove an entry (matches either layer; possessive-insensitive).
    /// Persists on save.
    #[allow(dead_code)]
    pub fn remove(&mut self, word: &str) -> bool {
        let c = canonical(word);
        let a = self.working_ci.remove(&c.to_lowercase());
        let b = self.working_cs.remove(&c);
        a || b
    }

    /// The merged phrase-bigram set (bundled Latin list + project phrases),
    /// used by the checker to accept tokens that are part of a known phrase.
    pub fn phrase_bigrams(&self) -> &HashSet<String> {
        &self.phrase_bigrams
    }

    /// Write the working dictionary back to disk.
    pub fn save_working(&self) -> anyhow::Result<()> {
        let dict = WorkingDict {
            ci: self.working_ci.clone(),
            cs: self.working_cs.clone(),
            phrases: self.working_phrases.clone(),
        };
        crate::dict::save(&self.working_path, &dict)
    }
}

/// Parse raw `.aff`/`.dic` text into a [`Dictionary`], mapping spellbook's
/// non-`std::error::Error` parse type into an `anyhow::Error`.
pub fn load_dictionary(aff: &str, dic: &str) -> anyhow::Result<Dictionary> {
    Dictionary::new(aff, dic).map_err(|e| anyhow::anyhow!("failed to parse dictionary: {e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::WorkingDict;
    use std::path::PathBuf;

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

    #[test]
    fn add_via_possessive_form_registers_stem() {
        // Focused on "Atrax's", pressing add should register the stem "Atrax",
        // which then accepts both "Atrax" and "Atrax's".
        let sys = crate::sysdict::resolve("en_US", None).unwrap();
        let dict = load_dictionary(&sys.aff, &sys.dic).unwrap();
        let wd = WorkingDict::default();
        let mut e = Engine::new(dict, wd, PathBuf::from("/dev/null"));
        e.add_ci("Atrax's");
        assert!(e.check("Atrax"));
        assert!(e.check("Atrax's"));
        assert!(e.check("atrax"));
    }

    #[test]
    fn ignore_via_possessive_suppresses_stem() {
        let sys = crate::sysdict::resolve("en_US", None).unwrap();
        let dict = load_dictionary(&sys.aff, &sys.dic).unwrap();
        let wd = WorkingDict::default();
        let mut e = Engine::new(dict, wd, PathBuf::from("/dev/null"));
        e.ignore_session("Atrax's");
        assert!(e.check("Atrax"));
        assert!(e.check("Atrax's"));
    }

    /// Regression guard for the local dict patch (en_US.dic: `else` -> `else/M`).
    /// Catches a re-vendor that drops the patch.
    #[test]
    fn vendored_dict_accepts_else_possessive() {
        let sys = crate::sysdict::resolve("en_US", None).unwrap();
        let dict = load_dictionary(&sys.aff, &sys.dic).unwrap();
        let e = Engine::new(dict, WorkingDict::default(), PathBuf::from("/dev/null"));
        assert!(e.check("else"));
        assert!(e.check("else's"), "local patch else->else/M missing?");
        assert!(e.check("anyone else's".split(' ').nth(1).unwrap()));
    }

    #[test]
    fn suggestions_drop_very_short() {
        let sys = crate::sysdict::resolve("en_US", None).unwrap();
        let dict = load_dictionary(&sys.aff, &sys.dic).unwrap();
        let e = Engine::new(dict, WorkingDict::default(), PathBuf::from("/dev/null"));
        // "se" would otherwise yield 1-2 char noise like "e", "s", "es".
        let sugs = e.suggest("se");
        assert!(sugs.iter().all(|s| s.chars().count() >= 3), "short leak: {sugs:?}");
        assert!(sugs.contains(&"see".to_string()));
    }
}
