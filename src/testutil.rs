//! Shared test helpers: per-test scratch directories with unique names and
//! RAII cleanup.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// A temporary directory removed (with everything in it) when dropped.
pub struct Scratch {
    pub dir: PathBuf,
}

impl Scratch {
    /// A path inside the scratch directory.
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Create a unique scratch directory (process id + atomic counter, so
/// parallel tests never collide) and clean it up at the end of the test.
pub fn scratch(label: &str) -> Scratch {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("redink-{label}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    Scratch { dir }
}
