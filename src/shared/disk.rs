//! Disk-backed storage: private workspaces, and read-only mappings of finished files.
use super::Res;
use memmap2::Mmap;
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static SEQ: AtomicU64 = AtomicU64::new(0);
/// A private directory removed on drop, including when a command bails out early.
pub struct Workspace {
    pub path: PathBuf,
    keep: bool,
}
impl Workspace {
    pub fn create(tmp_dir: &Path, keep: bool) -> Res<Self> {
        let path = tmp_dir.join(format!(
            "objtools-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self { path, keep })
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        if self.keep {
            eprintln!("Kept workspace: {}", self.path.display());
        } else {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
/// A file's directory, usable even when the path is a bare file name.
pub fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
/// Map a finished file read-only. Its pages are file-backed page cache: they stay resident
/// while RAM is free and are evicted, not swapped, under memory pressure.
pub fn map(path: &Path) -> Res<Mmap> {
    // SAFETY: callers map workspace spools once they are complete, or the buffers of a model
    // being inspected; nothing is expected to truncate or rewrite them while mapped.
    Ok(unsafe { Mmap::map(&File::open(path)?)? })
}
