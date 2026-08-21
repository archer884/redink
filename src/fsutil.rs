//! Atomic file replacement: write to a unique sibling temp file, fsync, then
//! rename over the target. A crash mid-write can never leave a truncated
//! manuscript or dictionary behind — the old content stays in place until
//! the new content is fully on disk.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_path(path: &Path) -> PathBuf {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "redink".to_string());
    parent.join(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Replace `path` with `bytes` atomically.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = temp_path(path);
    let result = (|| -> anyhow::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[test]
    fn replaces_content_and_leaves_no_temp_files() {
        let s = testutil::scratch("atomic");
        let p = s.path("f.md");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        let leftovers: Vec<_> = std::fs::read_dir(&s.dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn error_leaves_original_intact() {
        let s = testutil::scratch("atomic-err");
        let p = s.path("f.md");
        write_atomic(&p, b"original").unwrap();
        // A path whose parent is a regular file cannot be written.
        let bad = s.path("blocker").join("f.md");
        std::fs::write(s.path("blocker"), b"").unwrap();
        assert!(write_atomic(&bad, b"nope").is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"original");
    }
}
