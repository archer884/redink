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

impl WorkingDict {
    pub fn add_ci(&mut self, word: &str) -> bool {
        self.ci.insert(word.to_lowercase())
    }

    pub fn add_cs(&mut self, word: &str) -> bool {
        self.cs.insert(word.to_string())
    }

    pub fn remove(&mut self, word: &str) -> bool {
        self.ci.remove(&word.to_lowercase()) || self.cs.remove(word)
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
}
