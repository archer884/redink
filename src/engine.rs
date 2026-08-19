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
const CUSTOM_SUGGEST_LIMIT: usize = 3;

pub struct Engine {
    dict: Dictionary,
    custom_dict: Dictionary,
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
        let custom_dict = build_custom_dict(&working.ci, &working.cs);
        Self {
            dict,
            custom_dict,
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
    /// All-caps alphanumeric tokens (acronyms, model numbers, Roman numerals —
    /// "NASA", "M16", "XVII", including possessives like "NASA's") are accepted
    /// outright: they are almost always intentional and rarely spellcheckable.
    /// So are possessives of numbers ("1's", as tokenized from scene labels
    /// like "2/1's") and single non-ASCII letters ("Θ"), which are notation,
    /// not prose.
    ///
    /// The session and working-dictionary layers also honor possessive-stripping
    /// so that adding a coinage (e.g. "Thorne") also accepts its possessive
    /// ("Thorne's"), since the working dictionary is a plain set outside
    /// spellbook's `'s` affix machinery.
    pub fn check(&self, word: &str) -> bool {
        let stem = strip_possessive(word);
        if is_all_caps_alnum(word) || stem.is_some_and(is_all_caps_alnum) {
            return true;
        }
        if stem.is_some_and(|s| !s.chars().any(|c| c.is_alphabetic())) {
            return true;
        }
        if is_non_ascii_single_letter(word) || stem.is_some_and(is_non_ascii_single_letter) {
            return true;
        }
        if self.user_layer_has(word) || stem.is_some_and(|s| self.user_layer_has(s)) {
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

    /// Suggested corrections for `word`, with up to three working-dictionary
    /// results before system-dictionary results. Very short suggestions (< 3
    /// characters) are dropped — they're mostly noise.
    ///
    /// Possessive-aware, mirroring [`Engine::check`]: when `word` ends in
    /// `'s`, suggestions are also generated against the stem and the possessive
    /// is re-attached, so a misspelling like `Thryi's` surfaces `Thyri's` when
    /// `Thyri` is in the working dictionary. Without this, the stem is never
    /// consulted and a nearby coinage is invisible to the suggester even though
    /// `check` would have accepted it.
    pub fn suggest(&self, word: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Some(stem) = strip_possessive(word) {
            let suffix = &word[stem.len()..];
            for s in self.suggest_forms(stem) {
                let mut form = s;
                form.push_str(suffix);
                if !out.contains(&form) {
                    out.push(form);
                }
            }
        }
        for s in self.suggest_forms(word) {
            if !out.contains(&s) {
                out.push(s);
            }
        }
        out
    }

    /// Working-dictionary (up to three) then system-dictionary suggestions for
    /// a single form, with sub-`MIN_SUGGEST_LEN` results dropped.
    fn suggest_forms(&self, word: &str) -> Vec<String> {
        let mut custom = Vec::new();
        self.custom_dict
            .checker()
            .into_suggester()
            .suggest(word, &mut custom);
        custom.retain(|s| s.chars().count() >= MIN_SUGGEST_LEN);

        let mut out = custom
            .into_iter()
            .take(CUSTOM_SUGGEST_LIMIT)
            .collect::<Vec<_>>();

        let mut system = Vec::new();
        self.dict
            .checker()
            .into_suggester()
            .suggest(word, &mut system);
        system.retain(|s| s.chars().count() >= MIN_SUGGEST_LEN);
        for suggestion in system {
            if !out.contains(&suggestion) {
                out.push(suggestion);
            }
        }
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
        let word = canonical(word).to_lowercase();
        self.working_ci.insert(word.clone());
        let _ = self.custom_dict.add(&word);
    }

    /// Add a case-sensitive entry (exact casing only). The possessive stem is
    /// stored. Persists on save.
    pub fn add_cs(&mut self, word: &str) {
        let word = canonical(word);
        self.working_cs.insert(word.clone());
        let _ = self.custom_dict.add(&word);
    }

    /// Remove an entry (matches either layer; possessive-insensitive).
    /// Persists on save.
    #[allow(dead_code)]
    pub fn remove(&mut self, word: &str) -> bool {
        let c = canonical(word);
        let a = self.working_ci.remove(&c.to_lowercase());
        let b = self.working_cs.remove(&c);
        let removed = a || b;
        if removed {
            self.custom_dict = build_custom_dict(&self.working_ci, &self.working_cs);
        }
        removed
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

fn build_custom_dict(ci: &HashSet<String>, cs: &HashSet<String>) -> Dictionary {
    let mut dict = Dictionary::new("", "0\n").expect("empty dictionary is valid");
    for word in ci.iter().chain(cs.iter()) {
        let _ = dict.add(word);
    }
    dict
}

/// True for tokens made entirely of ASCII uppercase letters and digits with at
/// least one letter ("XVII", "NASA", "M16", "3M"). These are acronyms, model
/// numbers, or Roman numerals and are skipped by default.
fn is_all_caps_alnum(word: &str) -> bool {
    let mut has_letter = false;
    word.chars().all(|c| {
        if c.is_ascii_uppercase() {
            has_letter = true;
            true
        } else {
            c.is_ascii_digit()
        }
    }) && has_letter
}

/// True for a single non-ASCII letter ("Θ", "é", "ß"). Isolated foreign
/// alphabet characters are names or notation, not misspellings.
fn is_non_ascii_single_letter(word: &str) -> bool {
    let mut chars = word.chars();
    matches!(chars.next(), Some(c) if c.is_alphabetic() && !c.is_ascii()) && chars.next().is_none()
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
        let sys = crate::sysdict::resolve_embedded();
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
        let sys = crate::sysdict::resolve_embedded();
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
        let sys = crate::sysdict::resolve_embedded();
        let dict = load_dictionary(&sys.aff, &sys.dic).unwrap();
        let wd = WorkingDict::default();
        let mut e = Engine::new(dict, wd, PathBuf::from("/dev/null"));
        e.ignore_session("Atrax's");
        assert!(e.check("Atrax"));
        assert!(e.check("Atrax's"));
    }

    /// Regression guard for the local dict patch (en_US.patches:
    /// `else -> else/M`). Catches a re-vendor that drops the patch.
    #[test]
    fn vendored_dict_accepts_else_possessive() {
        let sys = crate::sysdict::resolve_embedded();
        let dict = load_dictionary(&sys.aff, &sys.dic).unwrap();
        let e = Engine::new(dict, WorkingDict::default(), PathBuf::from("/dev/null"));
        assert!(e.check("else"));
        assert!(e.check("else's"), "local patch else->else/M missing?");
        assert!(e.check("anyone else's".split(' ').nth(1).unwrap()));
    }

    /// Regression guard for the local dict patch (en_US.patches:
    /// `saddler/S -> saddler/SM`). Catches a re-vendor that drops the patch.
    #[test]
    fn vendored_dict_accepts_saddler_possessive() {
        let sys = crate::sysdict::resolve_embedded();
        let dict = load_dictionary(&sys.aff, &sys.dic).unwrap();
        let e = Engine::new(dict, WorkingDict::default(), PathBuf::from("/dev/null"));
        assert!(e.check("saddler"));
        assert!(e.check("saddler's"), "patch saddler/S->saddler/SM missing?");
        assert!(e.check("saddlers"), "plural (S flag) lost in patching?");
    }

    #[test]
    fn all_caps_alnum_accepted() {
        let sys = crate::sysdict::resolve_embedded();
        let dict = load_dictionary(&sys.aff, &sys.dic).unwrap();
        let e = Engine::new(dict, WorkingDict::default(), PathBuf::from("/dev/null"));
        for w in [
            "XVII",
            "NASA",
            "M16",
            "3M",
            "R2D2",
            "NASA's",
            "NASA\u{2019}S",
        ] {
            assert!(e.check(w), "{w} should be skipped as all-caps alnum");
        }
        // Mixed case and lowercase still go through the dictionaries.
        assert!(!e.check("Ml6"));
        assert!(!e.check("nasaa"));
    }

    /// Numeric possessives ("1's" as tokenized from "2/1's") can't be
    /// misspellings; letter-bearing stems are still checked.
    #[test]
    fn numeric_possessives_accepted() {
        let sys = crate::sysdict::resolve_embedded();
        let dict = load_dictionary(&sys.aff, &sys.dic).unwrap();
        let e = Engine::new(dict, WorkingDict::default(), PathBuf::from("/dev/null"));
        for w in ["0's", "1's", "5's", "42's", "42\u{2019}s"] {
            assert!(e.check(w), "{w} should be skipped as a numeric possessive");
        }
        assert!(!e.check("1x's"));
    }

    /// Single non-ASCII letters ("Θ") are notation, not typos. Multi-character
    /// non-ASCII words and ASCII single letters still go through the dictionary.
    #[test]
    fn non_ascii_single_letters_accepted() {
        let sys = crate::sysdict::resolve_embedded();
        let dict = load_dictionary(&sys.aff, &sys.dic).unwrap();
        let e = Engine::new(dict, WorkingDict::default(), PathBuf::from("/dev/null"));
        for w in ["Θ", "é", "Æ", "Ω"] {
            assert!(
                e.check(w),
                "{w} should be skipped as a single non-ASCII letter"
            );
        }
        assert!(!e.check("ΘΘ"));
        assert!(e.check("a"), "'a' is a real word");
    }

    #[test]
    fn suggestions_drop_very_short() {
        let sys = crate::sysdict::resolve_embedded();
        let dict = load_dictionary(&sys.aff, &sys.dic).unwrap();
        let e = Engine::new(dict, WorkingDict::default(), PathBuf::from("/dev/null"));
        // "se" would otherwise yield 1-2 char noise like "e", "s", "es".
        let sugs = e.suggest("se");
        assert!(
            sugs.iter().all(|s| s.chars().count() >= 3),
            "short leak: {sugs:?}"
        );
        assert!(sugs.contains(&"see".to_string()));
    }

    #[test]
    fn suggestions_include_working_dictionary() {
        let e = engine_with_ci("Thorne");
        let sugs = e.suggest("Thorn");
        assert!(
            sugs.contains(&"Thorne".to_string()),
            "custom suggestion missing: {sugs:?}"
        );
    }

    #[test]
    fn suggestions_include_entries_added_after_startup() {
        let sys = crate::sysdict::resolve_embedded();
        let dict = load_dictionary(&sys.aff, &sys.dic).unwrap();
        let mut e = Engine::new(dict, WorkingDict::default(), PathBuf::from("/dev/null"));
        e.add_cs("Gondor");
        let sugs = e.suggest("gondr");
        assert!(
            sugs.contains(&"Gondor".to_string()),
            "custom suggestion missing: {sugs:?}"
        );
    }

    /// Regression: a misspelled possessive must surface a working-dict stem
    /// with the possessive re-attached. Before the fix, `suggest` did not
    /// strip the possessive (unlike `check`), so `Thryi's` could not see
    /// `Thyri` and the user registered the typo `Thryi` as a coinage.
    #[test]
    fn suggest_possessive_finds_ci_stem() {
        let e = engine_with_ci("Thyri");
        let sugs = e.suggest("Thryi's");
        assert!(
            sugs.contains(&"Thyri's".to_string()),
            "expected Thyri's (re-attached) in {sugs:?}"
        );
    }

    #[test]
    fn suggest_possessive_finds_cs_stem() {
        let sys = crate::sysdict::resolve_embedded();
        let dict = load_dictionary(&sys.aff, &sys.dic).unwrap();
        let mut e = Engine::new(dict, WorkingDict::default(), PathBuf::from("/dev/null"));
        e.add_cs("Thyri");
        let sugs = e.suggest("Thryi's");
        assert!(
            sugs.contains(&"Thyri's".to_string()),
            "expected Thyri's (re-attached) in {sugs:?}"
        );
    }

    /// The re-attached form should rank ahead of unrelated full-token matches,
    /// since the possessive form is the most likely intent.
    #[test]
    fn suggest_possessive_prioritizes_stem_derived() {
        let e = engine_with_ci("Thyri");
        let sugs = e.suggest("Thryi's");
        let stem_idx = sugs.iter().position(|s| s == "Thyri's");
        let other_idx = sugs.iter().position(|s| s == "Thrift's");
        assert!(stem_idx.is_some(), "Thyri's missing: {sugs:?}");
        if let (Some(stem), Some(other)) = (stem_idx, other_idx) {
            assert!(stem < other, "stem-derived should precede system: {sugs:?}");
        }
    }
}
