//! Temp-file bookkeeping shared by the subcommands that spool to disk.

use std::path::{Path, PathBuf};

/// Tracks temp files and removes them on drop — including when a subcommand bails out early,
/// which matters when the spools are the size of the input.
pub struct TmpGuard {
    paths: Vec<PathBuf>,
    keep: bool,
}

impl TmpGuard {
    pub fn new(keep: bool) -> Self {
        Self { paths: Vec::new(), keep }
    }

    pub fn track(&mut self, p: PathBuf) -> &PathBuf {
        self.paths.push(p);
        self.paths.last().unwrap()
    }

    /// Remove one tracked file now, rather than waiting for the drop. Used when spools are
    /// per-unit-of-work and would otherwise all pile up at once.
    pub fn release(&mut self, p: &Path) {
        if self.keep {
            return;
        }
        if let Some(i) = self.paths.iter().position(|x| x == p) {
            self.paths.remove(i);
            let _ = std::fs::remove_file(p);
        }
    }
}

impl Drop for TmpGuard {
    fn drop(&mut self) {
        if !self.keep {
            for p in &self.paths {
                let _ = std::fs::remove_file(p);
            }
        }
    }
}
