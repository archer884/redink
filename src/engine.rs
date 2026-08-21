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

use crate::dict::{PhraseBigrams, WorkingDict, canonical, strip_possessive};

/// Minimum character length for a suggestion to be shown. Shorter ones are
/// almost always noise (e.g. "e", "s", "es").
const MIN_SUGGEST_LEN: usize = 3;
const CUSTOM_SUGGEST_LIMIT: usize = 3;

pub struct Engine {
    dict: Dictionary,
    custom_dict: Dictionary,
    working_path: PathBuf,
    working: WorkingDict,
    session_ignore: HashSet<String>,
    phrase_bigrams: PhraseBigrams,
    phrase_bigrams_cs: PhraseBigrams,
}

impl Engine {
    pub fn new(dict: Dictionary, working: WorkingDict, working_path: PathBuf) -> Self {
        let custom_dict = build_custom_dict(&working.ci, &working.cs);
        let mut this = Self {
            dict,
            custom_dict,
            working_path,
            working,
            session_ignore: HashSet::new(),
            phrase_bigrams: PhraseBigrams::default(),
            phrase_bigrams_cs: PhraseBigrams::default(),
        };
        this.rebuild_phrase_bigrams();
        this
    }

    pub fn working_path(&self) -> &std::path::Path {
        &self.working_path
    }

    /// Number of word entries (CI + CS layers) in the working dictionary.
    pub fn working_word_count(&self) -> usize {
        self.working.ci.len() + self.working.cs.len()
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
        let lower = word.to_lowercase();
        self.session_ignore.contains(&lower)
            || self.working.cs.contains(word)
            || self.working.ci.contains(&lower)
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
        let stored = canonical(word).to_lowercase();
        if self.working.add_ci(word) {
            let _ = self.custom_dict.add(&stored);
        }
    }

    /// Add a case-sensitive entry (exact casing only). The possessive stem is
    /// stored. Persists on save.
    pub fn add_cs(&mut self, word: &str) {
        let stored = canonical(word);
        if self.working.add_cs(word) {
            let _ = self.custom_dict.add(&stored);
        }
    }

    /// Add a word or phrase (`p` in the TUI). A single word routes to
    /// [`add_ci`](Self::add_ci)/[`add_cs`](Self::add_cs); a multi-word entry
    /// becomes a phrase on the ci or cs phrase layer (bigram-matched against
    /// neighbours, exactly like the bundled Latin phrases). Phrase bigram
    /// sets are rebuilt so the change takes effect immediately. Persists on
    /// save.
    pub fn add_phrase(&mut self, text: &str, sensitive: bool) {
        let norm: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if norm.is_empty() {
            return;
        }
        let is_phrase = norm.chars().any(char::is_whitespace);
        match self.working.add_entry(text, sensitive) {
            crate::dict::AddOutcome::Added if is_phrase => self.rebuild_phrase_bigrams(),
            crate::dict::AddOutcome::Added => {
                let stored = if sensitive {
                    canonical(text)
                } else {
                    canonical(text).to_lowercase()
                };
                let _ = self.custom_dict.add(&stored);
            }
            _ => {}
        }
    }

    /// Rebuild the merged ci phrase-bigram index and the cs phrase-bigram
    /// index from the working phrase layers.
    fn rebuild_phrase_bigrams(&mut self) {
        self.phrase_bigrams = crate::dict::build_phrase_bigrams(&self.working.phrases);
        self.phrase_bigrams_cs = crate::dict::PhraseBigrams::from_phrases(
            self.working.phrases_cs.iter().map(String::as_str),
        );
    }

    /// Remove an entry (matches either layer, words or phrases;
    /// possessive-insensitive). Persists on save.
    #[allow(dead_code)]
    pub fn remove(&mut self, word: &str) -> bool {
        let removed = self.working.remove(word);
        if removed {
            self.custom_dict = build_custom_dict(&self.working.ci, &self.working.cs);
            self.rebuild_phrase_bigrams();
        }
        removed
    }

    /// The merged phrase-bigram index (bundled Latin list + project
    /// phrases), used by the checker to accept tokens that are part of a
    /// known phrase.
    pub fn phrase_bigrams(&self) -> &PhraseBigrams {
        &self.phrase_bigrams
    }

    /// Exact-casing phrase bigrams from the working dictionary: a token is
    /// covered only when the neighbouring words match the phrase as written.
    pub fn phrase_bigrams_cs(&self) -> &PhraseBigrams {
        &self.phrase_bigrams_cs
    }

    /// Write the working dictionary back to disk.
    pub fn save_working(&self) -> anyhow::Result<()> {
        crate::dict::save(&self.working_path, &self.working)
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
/// numbers, or Roman numerals and are skipped by default. Note this includes
/// single capitals ("B", "K") — initials and notation, not misspellings — by
/// design.
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

    /// `p` in the TUI: single words route to the word layers, phrases land on
    /// the phrase layers with bigrams rebuilt immediately, and everything
    /// round-trips through save_working.
    #[test]
    fn add_phrase_layers_and_roundtrip() {
        let s = crate::testutil::scratch("addphrase");
        let path = s.path("t.dic");
        let _ = std::fs::remove_file(&path);

        let sys = crate::sysdict::resolve_embedded();
        let dict = load_dictionary(&sys.aff, &sys.dic).unwrap();
        let mut e = Engine::new(dict, WorkingDict::default(), path.clone());

        // Single word: behaves like add_ci (and is checked immediately).
        e.add_phrase("hobbit", false);
        assert!(e.check("Hobbit"));

        // Phrases: bigrams live, on the right layer.
        e.add_phrase("tzeya gan", false);
        assert!(
            e.phrase_bigrams().contains("tzeya", "gan"),
            "ci bigram missing after add_phrase"
        );
        e.add_phrase("Tzeya Gan", true);
        assert!(
            e.phrase_bigrams_cs().contains("Tzeya", "Gan"),
            "cs bigram missing after add_phrase"
        );

        // Whitespace is normalized: extra spaces still yield one clean entry.
        e.add_phrase("per   se", false);
        assert!(e.phrase_bigrams().contains("per", "se"));

        e.save_working().unwrap();
        let loaded = crate::dict::load(&path).unwrap();
        assert!(loaded.ci.contains("hobbit"));
        assert!(loaded.phrases.contains("tzeya gan"));
        assert!(loaded.phrases.contains("per se"));
        // "=Tzeya Gan" is shadowed by the CI phrase "tzeya gan", so save
        // prunes it from the file (the in-memory CS layer above is unaffected).
        assert!(!loaded.phrases_cs.contains("Tzeya Gan"));
        let _ = std::fs::remove_file(&path);
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
