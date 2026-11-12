//! Disk-backed storage: private workspaces, and read-only mappings of finished files.
use super::{
    failure::{Context, JobDirectory},
    Res,
};
use memmap2::Mmap;
use std::{
    collections::hash_map::RandomState,
    fs::{self, File, OpenOptions},
    hash::{BuildHasher, Hasher},
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};
/// A private directory removed on drop, including when a command bails out early.
pub struct Workspace {
    pub path: PathBuf,
    keep: bool,
}
impl Workspace {
    pub fn create(tmp_dir: &Path, keep: bool) -> Res<Self> {
        // The process ID alone repeats in every PID namespace, so a killed container's leftovers
        // would block the next job. It stays in the name to trace a leftover to its process.
        // `RandomState` is the standard library's OS-seeded randomness.
        let random = RandomState::new().build_hasher().finish();
        let path = tmp_dir.join(format!("objtools-{}-{random:016x}", std::process::id()));
        fs::create_dir(&path)
            .map_err(JobDirectory)
            .with_context(|| "Workspace directory".into())?;
        Ok(Self { path, keep })
    }
}
impl Workspace {
    /// A workspace removed under a running job shows up as a missing file, which would otherwise
    /// read as a reference in the model leading nowhere.
    pub fn explain<T>(&self, result: Res<T>) -> Res<T> {
        if result.is_err() && !self.path.is_dir() {
            let gone = io::Error::new(ErrorKind::NotFound, "removed while the job was running");
            return Err(JobDirectory(gone)).with_context(|| "Workspace directory".into());
        }
        result
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        if self.keep {
            crate::say!("Kept workspace: {}", self.path.display());
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
/// Tracks published paths so an error removes incomplete output, without touching existing files.
#[derive(Default)]
pub struct Outputs {
    paths: Vec<PathBuf>,
    committed: bool,
}
impl Outputs {
    pub fn create(&mut self, path: &Path) -> Res<File> {
        let f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(JobDirectory)
            .with_context(|| "Output file".into())?;
        self.paths.push(path.to_owned());
        Ok(f)
    }
    pub fn commit(&mut self) {
        self.committed = true;
    }
}
impl Drop for Outputs {
    fn drop(&mut self) {
        if !self.committed {
            for p in &self.paths {
                let _ = fs::remove_file(p);
            }
        }
    }
}
/// Map a finished file read-only. Its pages are file-backed page cache: they stay resident
/// while RAM is free and are evicted, not swapped, under memory pressure.
pub fn map(path: &Path) -> Res<Mmap> {
    // SAFETY: callers map workspace spools once they are complete, or the buffers of a model
    // being inspected; nothing is expected to truncate or rewrite them while mapped.
    Ok(unsafe { Mmap::map(&File::open(path)?)? })
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;
    fn fixture(name: &str) -> PathBuf {
        let dir = temp_dir().join(format!("objtools-disk-{name}-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        dir
    }
    #[test]
    fn workspaces_in_one_process_get_different_names() {
        let dir = fixture("distinct");
        let (a, b) = (
            Workspace::create(&dir, false).unwrap(),
            Workspace::create(&dir, false).unwrap(),
        );
        assert_ne!(a.path, b.path);
        drop((a, b));
        fs::remove_dir_all(&dir).unwrap();
    }
}
