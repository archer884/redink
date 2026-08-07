//! Locating the system (base) dictionary across macOS and Linux.
//!
//! Load order, resolved in [`resolve`]:
//! 1. `--sysdict-dir` (an explicit directory containing `<lang>.aff`/`.dic`)
//! 2. Standard system search paths for `<lang>`
//! 3. The vendored SCOWL `en_US` embedded in the binary (fallback for `en_US`)

use std::path::{Path, PathBuf};

/// Directories searched, in order, on both macOS and Linux.
fn system_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = vec![
        // Linux
        "/usr/share/hunspell".into(),
        "/usr/share/myspell".into(),
        "/usr/share/myspell/dicts".into(),
        "/usr/local/share/hunspell".into(),
        "/usr/lib/hunspell".into(),
        // macOS
        "/Library/Spelling".into(),
        "/opt/homebrew/share/hunspell".into(),
        "/usr/local/share/hunspell".into(),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        v.push(PathBuf::from(home).join("Library/Spelling"));
    }
    v
}

/// A resolved dictionary: its contents plus a human-readable provenance label.
pub struct SystemDict {
    pub aff: String,
    pub dic: String,
    pub source: String,
}

/// Locate `<lang>.aff`/`.dic` under an explicit override directory.
pub fn locate(lang: &str, override_dir: Option<&Path>) -> Option<(PathBuf, PathBuf)> {
    let dirs: Vec<PathBuf> = match override_dir {
        Some(d) => vec![d.to_path_buf()],
        None => system_dirs(),
    };
    for d in dirs {
        let aff = d.join(format!("{lang}.aff"));
        let dic = d.join(format!("{lang}.dic"));
        if aff.is_file() && dic.is_file() {
            return Some((aff, dic));
        }
    }
    None
}

const EMBEDDED_AFF: &str = include_str!("../assets/dict/en_US.aff");
const EMBEDDED_DIC: &str = include_str!("../assets/dict/en_US.dic");

/// Resolve the base dictionary per the load order, falling back to the
/// embedded `en_US` when nothing else is found.
pub fn resolve(lang: &str, override_dir: Option<&Path>) -> anyhow::Result<SystemDict> {
    if let Some((aff_path, dic_path)) = locate(lang, override_dir) {
        let aff = std::fs::read_to_string(&aff_path)?;
        let dic = std::fs::read_to_string(&dic_path)?;
        return Ok(SystemDict {
            aff,
            dic,
            source: format!("{}", aff_path.with_extension("").display()),
        });
    }

    if lang == "en_US" {
        return Ok(SystemDict {
            aff: EMBEDDED_AFF.to_string(),
            dic: EMBEDDED_DIC.to_string(),
            source: "embedded (SCOWL en_US 2020.12.07, size 60)".to_string(),
        });
    }

    anyhow::bail!(
        "no dictionary found for language '{lang}'.

redink searched: {}
To install one (en_US example):
  Arch:   sudo pacman -S hunspell-en_us
  Debian: sudo apt install hunspell-en-us
  macOS:  brew install hunspell
Or point at a directory with {lang}.aff and {lang}.dic via --sysdict-dir <DIR>",
        system_dirs()
            .iter()
            .map(|d| format!("{}", d.display()))
            .collect::<Vec<_>>()
            .join(", "),
    )
}
