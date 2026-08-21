//! The working (project) dictionary: a plain-text, git-friendly wordlist that
//! augments the system dictionary. Additions from the TUI/CLI are written here
//! and only here — the system dictionary is never modified.
//!
//! File format (UTF-8, one entry per line):
//! ```text
//! # redink working dictionary
//! # bare word      -> case-insensitive (accepted in any case; stored lowercase)
//! # =Word          -> case-sensitive   (accepted only in that exact casing)
//! # multi-word line -> phrase (bigram-matched, case-insensitive)
//! # =Multi Word     -> phrase, exact casing only
//! hobbit
//! elflord
//! =Gondor
//! per se
//! =Tzeya Gan
//! ```
//! Lines starting with `#` are comments; blank lines are ignored.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

const DEFAULT_NAME: &str = ".redink.dic";
const HEADER: &[&str] = &[
    "# redink working dictionary",
    "# bare word: case-insensitive  |  =Word: case-sensitive  |  multi-word line: phrase (= prefix: exact case)",
];

/// Common Latin phrases used in English prose. A token is accepted when it
/// forms part of one of these (bigram-matched against its neighbours), so
/// e.g. "se" passes only in "per se" and is still flagged elsewhere. Extend
/// per-project by adding multi-word lines to `.redink.dic`.
pub const LATIN_PHRASES: &[&str] = &[
    "a fortiori",
    "a posteriori",
    "a priori",
    "ad hoc",
    "ad hominem",
    "ad lib",
    "ad libitum",
    "bona fide",
    "caveat emptor",
    "corpus delicti",
    "de facto",
    "de jure",
    "et cetera",
    "ex ante",
    "ex cathedra",
    "ex officio",
    "ex parte",
    "ex post",
    "ex post facto",
    "in extremis",
    "in re",
    "in situ",
    "in vitro",
    "in vivo",
    "inter alia",
    "ipso facto",
    "magnum opus",
    "modus operandi",
    "mutatis mutandis",
    "non sequitur",
    "nota bene",
    "per se",
    "prima facie",
    "pro rata",
    "pro se",
    "quid pro quo",
    "res judicata",
    "status quo",
    "sub judice",
    "sub rosa",
    "sui generis",
    "tabula rasa",
    "vice versa",
];

#[derive(Debug, Default, Clone)]
pub struct WorkingDict {
    /// Case-insensitive entries, stored lowercase.
    pub ci: HashSet<String>,
    /// Case-sensitive entries, stored in exact casing.
    pub cs: HashSet<String>,
    /// Multi-word phrases (lowercased), matched as bigrams against neighbours.
    pub phrases: HashSet<String>,
    /// Exact-casing phrases, matched as bigrams against neighbours as written.
    pub phrases_cs: HashSet<String>,
}

/// Decompose a phrase into its adjacent bigrams, lowercased and
/// whitespace-normalized. "quid pro quo" -> {"quid pro", "pro quo"}.
pub fn phrase_bigrams(phrase: &str) -> Vec<String> {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    words
        .windows(2)
        .map(|w| format!("{} {}", w[0], w[1]))
        .collect()
}

/// Build the full bigram set used for phrase matching: the bundled Latin list
/// plus any project phrases from the working dictionary.
pub fn build_phrase_bigrams(user_phrases: &HashSet<String>) -> HashSet<String> {
    let mut set = HashSet::new();
    let all = LATIN_PHRASES
        .iter()
        .copied()
        .chain(user_phrases.iter().map(String::as_str));
    for p in all {
        for bg in phrase_bigrams(p) {
            set.insert(bg);
        }
    }
    set
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

/// Outcome of a working-dictionary add: a word may be newly inserted, already
/// present (same layer), or ignored as a malformed phrase (a multi-word entry
/// that collapses to fewer than two tokens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    Added,
    Duplicate,
    Ignored,
}

impl WorkingDict {
    /// Add a word (or phrase) on the appropriate layer, returning whether it
    /// was newly inserted, already present (same layer), or ignored as a
    /// malformed phrase (a multi-word entry that collapses to fewer than two
    /// tokens). Multi-word entries go to `phrases` (lowercased) or, with
    /// `sensitive`, to `phrases_cs` (exact casing).
    pub fn add_entry(&mut self, word: &str, sensitive: bool) -> AddOutcome {
        if word.chars().any(char::is_whitespace) {
            let norm: String = word.split_whitespace().collect::<Vec<_>>().join(" ");
            if norm.split(' ').count() >= 2 {
                let added = if sensitive {
                    self.phrases_cs.insert(norm)
                } else {
                    self.phrases.insert(norm.to_lowercase())
                };
                if added {
                    AddOutcome::Added
                } else {
                    AddOutcome::Duplicate
                }
            } else {
                AddOutcome::Ignored
            }
        } else if sensitive {
            if self.add_cs(word) {
                AddOutcome::Added
            } else {
                AddOutcome::Duplicate
            }
        } else if self.add_ci(word) {
            AddOutcome::Added
        } else {
            AddOutcome::Duplicate
        }
    }

    pub fn add_ci(&mut self, word: &str) -> bool {
        self.ci.insert(canonical(word).to_lowercase())
    }

    pub fn add_cs(&mut self, word: &str) -> bool {
        self.cs.insert(canonical(word))
    }

    /// Remove an entry (matches either case layer, words or phrases;
    /// possessive-insensitive).
    pub fn remove(&mut self, word: &str) -> bool {
        let c = canonical(word);
        if self.ci.remove(&c.to_lowercase()) || self.cs.remove(&c) {
            return true;
        }
        if c.chars().any(char::is_whitespace) {
            let norm: String = c.split_whitespace().collect::<Vec<_>>().join(" ");
            if norm.split(' ').count() >= 2 {
                return self.phrases_cs.remove(&norm) || self.phrases.remove(&norm.to_lowercase());
            }
        }
        false
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.ci.is_empty()
            && self.cs.is_empty()
            && self.phrases.is_empty()
            && self.phrases_cs.is_empty()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.ci.len() + self.cs.len() + self.phrases.len() + self.phrases_cs.len()
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
        // A `=` prefix marks case-sensitive; a line containing whitespace is
        // a phrase (matched as bigrams), so `=Tzeya Gan` is an exact-casing
        // phrase.
        let (body, sensitive) = match line.strip_prefix('=') {
            Some(rest) => (rest, true),
            None => (line, false),
        };
        if body.chars().any(char::is_whitespace) {
            let norm: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
            if norm.split(' ').count() >= 2 {
                if sensitive {
                    dict.phrases_cs.insert(norm);
                } else {
                    dict.phrases.insert(norm.to_lowercase());
                }
            }
            continue;
        }
        if sensitive {
            if !body.is_empty() {
                dict.add_cs(body);
            }
        } else {
            dict.add_ci(body);
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
    let mut ph: Vec<&String> = dict.phrases.iter().collect();
    ph.sort();
    let mut phcs: Vec<&String> = dict.phrases_cs.iter().collect();
    phcs.sort();

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
    if !ph.is_empty() || !phcs.is_empty() {
        out.push_str("# phrases:\n");
        for w in &ph {
            out.push_str(w);
            out.push('\n');
        }
        for w in &phcs {
            out.push('=');
            out.push_str(w);
            out.push('\n');
        }
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
    fn add_entry_reports_outcome() {
        let mut d = WorkingDict::default();
        // case-insensitive: first add is new, second is a duplicate.
        assert_eq!(d.add_entry("hobbit", false), AddOutcome::Added);
        assert_eq!(d.add_entry("hobbit", false), AddOutcome::Duplicate);
        // canonicalization: adding the possessive form finds the stem present.
        assert_eq!(d.add_entry("Hobbit's", false), AddOutcome::Duplicate);
        // a differently-cased token still hits the same CI entry.
        assert_eq!(d.add_entry("HOBBIT", false), AddOutcome::Duplicate);

        // case-sensitive layer is independent (no cross-layer dedup).
        assert_eq!(d.add_entry("Gondor", true), AddOutcome::Added);
        assert_eq!(d.add_entry("Gondor", true), AddOutcome::Duplicate);
        // CI and CS don't shadow each other: adding the CS form is still new.
        let mut d2 = WorkingDict::default();
        d2.add_ci("gondor");
        assert_eq!(d2.add_entry("Gondor", true), AddOutcome::Added);

        // phrases.
        assert_eq!(d.add_entry("quid pro quo", false), AddOutcome::Added);
        assert_eq!(d.add_entry("quid pro quo", false), AddOutcome::Duplicate);
        // a multi-word argument that collapses to <2 tokens is ignored.
        assert_eq!(d.add_entry("   ", false), AddOutcome::Ignored);

        // exact-case phrases live on their own layer.
        assert_eq!(d.add_entry("Tzeya Gan", true), AddOutcome::Added);
        assert_eq!(d.add_entry("Tzeya Gan", true), AddOutcome::Duplicate);
        assert!(d.phrases_cs.contains("Tzeya Gan"));
        assert!(!d.phrases.contains("tzeya gan"));
        // the same words on the case-insensitive layer are a separate entry.
        assert_eq!(d.add_entry("tzeya gan", false), AddOutcome::Added);
    }

    #[test]
    fn remove_canonicalizes_possessive() {
        let mut d = WorkingDict::default();
        d.add_ci("atrax");
        assert!(d.remove("Atrax's")); // remove via the possessive form
        assert!(!d.ci.contains("atrax"));
    }

    #[test]
    fn phrase_load_save_roundtrip() {
        let p = scratch("phrases.dic");
        let mut d = WorkingDict::default();
        d.phrases.insert("per se".to_string());
        d.phrases.insert("quid pro quo".to_string());
        d.phrases_cs.insert("Tzeya Gan".to_string());
        save(&p, &d).unwrap();
        let loaded = load(&p).unwrap();
        assert!(loaded.phrases.contains("per se"));
        assert!(loaded.phrases.contains("quid pro quo"));
        assert!(loaded.phrases_cs.contains("Tzeya Gan"));
        assert!(
            !loaded.phrases.contains("=tzeya gan"),
            "= must not be baked into the phrase"
        );
    }

    /// A hand-written `=Tzeya Gan` line parses as an exact-case phrase, not a
    /// literal "=..." word (regression: the prefix used to be baked in).
    #[test]
    fn phrase_cs_prefix_parses() {
        let p = scratch("cs-phrase.dic");
        std::fs::write(&p, "hobbit\n=Gondor\nper se\n=Tzeya Gan\n").unwrap();
        let d = load(&p).unwrap();
        assert!(d.ci.contains("hobbit"));
        assert!(d.cs.contains("Gondor"));
        assert!(d.phrases.contains("per se"));
        assert!(d.phrases_cs.contains("Tzeya Gan"));
    }

    #[test]
    fn remove_phrase() {
        let mut d = WorkingDict::default();
        d.add_entry("Tzeya Gan", true);
        assert!(d.remove("Tzeya Gan"));
        assert!(d.phrases_cs.is_empty());
        d.add_entry("quid pro quo", false);
        // remove matches either phrase layer, case-insensitively for ci.
        assert!(d.remove("Quid Pro Quo"));
        assert!(d.phrases.is_empty());
        // an unknown phrase removes nothing.
        assert!(!d.remove("per se"));
    }

    #[test]
    fn phrase_bigrams_decompose() {
        let bg = phrase_bigrams("quid pro quo");
        assert_eq!(bg, vec!["quid pro", "pro quo"]);
        assert_eq!(phrase_bigrams("per se"), vec!["per se"]);
        assert!(phrase_bigrams("solo").is_empty());
    }
}
