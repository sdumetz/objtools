//! The one way to follow a file reference written inside a model.
use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};
/// One answer for a missing file and a forbidden one, so a reference can't probe what exists
/// outside the model's folder.
#[derive(Debug)]
pub struct Refused;
impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("not found, or outside the model's folder")
    }
}
impl Error for Refused {}
#[derive(Clone)]
pub struct References {
    /// The canonical directory references must stay in. `None` follows them anywhere.
    root: Option<PathBuf>,
}
impl References {
    /// What the model in `dir` may reach, given whether the user has vouched for it.
    pub fn for_input(dir: &Path, trusted: bool) -> io::Result<Self> {
        if trusted {
            Ok(Self::unrestricted())
        } else {
            Self::confined(dir)
        }
    }
    /// For input the user hasn't vouched for: only regular files beneath `dir` can be reached.
    pub fn confined(dir: &Path) -> io::Result<Self> {
        Ok(Self {
            root: Some(fs::canonicalize(non_empty(dir))?),
        })
    }
    /// For `--trusted`, and for objtools reading back intermediates it wrote from input it had
    /// already confined. References in these may point anywhere, and open errors are the real ones.
    pub fn unrestricted() -> Self {
        Self { root: None }
    }
    /// Where `reference`, written in a file in `base`, leads. A URI must be decoded first.
    pub fn resolve(&self, base: &Path, reference: &str) -> Result<PathBuf, Refused> {
        let base = non_empty(base);
        let written = reference.replace('\\', "/");
        let Some(root) = &self.root else {
            return Ok(base.join(written));
        };
        if !looks_relative(&written) {
            return Err(Refused);
        }
        // Decided from the text alone, so the path of a refused reference is never looked up.
        let base = fs::canonicalize(base).map_err(|_| Refused)?;
        let depth = base.strip_prefix(root).map_err(|_| Refused)?;
        if climbs_out(&written, depth.components().count()) {
            return Err(Refused);
        }
        regular_file_inside(root, &base.join(written))
    }
}
fn non_empty(dir: &Path) -> &Path {
    if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    }
}
/// A relative path: no root, drive letter, URI scheme or NUL byte.
fn looks_relative(written: &str) -> bool {
    // Windows also reads a colon as an alternate data stream, or as a drive.
    let colon = cfg!(windows) && written.contains(':');
    !(written.is_empty()
        || written.contains('\0')
        || written.starts_with('/')
        || colon
        || has_prefix(written))
}
/// `C:`, `http:` and the like. A drive letter is the one-letter case of a scheme.
fn has_prefix(written: &str) -> bool {
    written.split_once(':').is_some_and(|(prefix, _)| {
        prefix.starts_with(|c: char| c.is_ascii_alphabetic())
            && prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}
/// Whether the text reaches above the directory `depth` levels below the root. This reads the text
/// only: symlinks are for `regular_file_inside` to catch.
fn climbs_out(written: &str, mut depth: usize) -> bool {
    for part in written.split('/') {
        match part {
            "" | "." => {}
            ".." if depth == 0 => return true,
            ".." => depth -= 1,
            _ => depth += 1,
        }
    }
    false
}
/// The kernel's view of the path, which is what catches `link-to-outside/../file`. The type check
/// comes before any open, because opening a FIFO would block.
fn regular_file_inside(root: &Path, candidate: &Path) -> Result<PathBuf, Refused> {
    let file = fs::canonicalize(candidate).map_err(|_| Refused)?;
    let regular = fs::metadata(&file).is_ok_and(|m| m.is_file());
    if regular && file.starts_with(root) {
        Ok(file)
    } else {
        Err(Refused)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// `job` holds the model, with `tex/a.png` and `m.bin`. `outside/s.txt` sits next to it.
    struct Fixture(PathBuf);
    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("objtools-references-{name}-{}", std::process::id()));
            fs::create_dir_all(dir.join("job/tex")).unwrap();
            fs::create_dir_all(dir.join("outside")).unwrap();
            fs::write(dir.join("job/tex/a.png"), "a").unwrap();
            fs::write(dir.join("job/m.bin"), "m").unwrap();
            fs::write(dir.join("outside/s.txt"), "s").unwrap();
            Self(dir)
        }
        fn job(&self) -> PathBuf {
            self.0.join("job")
        }
        fn found(&self, base: &str, reference: &str) -> Option<PathBuf> {
            References::confined(&self.job())
                .unwrap()
                .resolve(&self.job().join(base), reference)
                .ok()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn references_inside_are_found() {
        let f = Fixture::new("inside");
        let a = fs::canonicalize(f.job().join("tex/a.png")).unwrap();
        let m = fs::canonicalize(f.job().join("m.bin")).unwrap();
        assert_eq!(f.found("", "tex/a.png").unwrap(), a);
        assert_eq!(f.found("", "tex\\a.png").unwrap(), a);
        assert_eq!(f.found("", "./tex/../m.bin").unwrap(), m);
        assert_eq!(f.found("tex", "a.png").unwrap(), a);
        assert_eq!(f.found("tex", "../m.bin").unwrap(), m);
    }
    #[test]
    fn references_leaving_are_refused() {
        let f = Fixture::new("leaving");
        for reference in [
            "../outside/s.txt",
            "tex/../../outside/s.txt",
            "..\\outside\\s.txt",
            "..",
            "tex/../..",
        ] {
            assert_eq!(f.found("", reference), None, "{reference}");
        }
        assert_eq!(f.found("tex", "../../outside/s.txt"), None);
    }
    #[test]
    fn absolute_and_remote_references_are_refused() {
        let f = Fixture::new("absolute");
        let absolute = fs::canonicalize(f.job().join("m.bin")).unwrap();
        let absolute = absolute.to_str().unwrap();
        for reference in [
            absolute,
            "/etc/passwd",
            "C:\\Windows\\win.ini",
            "C:/Windows/win.ini",
            "C:win.ini",
            "\\\\server\\share\\a.png",
            "file:///etc/passwd",
            "http://x/y.png",
        ] {
            assert_eq!(f.found("", reference), None, "{reference}");
        }
    }
    #[test]
    fn what_is_not_a_file_is_refused() {
        let f = Fixture::new("files");
        for reference in ["", ".", "tex", "tex/", "missing.png", "m.bin/x", "a\0b"] {
            assert_eq!(f.found("", reference), None, "{reference:?}");
        }
    }
    #[test]
    fn a_colon_is_only_a_prefix_at_the_start() {
        assert!(has_prefix("C:\\x"));
        assert!(has_prefix("http://x"));
        assert!(!has_prefix("12:30.png"));
        assert!(!has_prefix("tex/a:b.png"));
        assert!(!has_prefix("a.png"));
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_are_judged_by_where_they_lead() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new("symlinks");
        symlink(f.job().join("tex/a.png"), f.job().join("inner-link")).unwrap();
        symlink(f.0.join("outside/s.txt"), f.job().join("file-link")).unwrap();
        symlink(f.0.join("outside"), f.job().join("dir-link")).unwrap();
        symlink("/etc", f.job().join("etc-link")).unwrap();
        symlink("loop", f.job().join("loop")).unwrap();
        assert!(f.found("", "inner-link").is_some());
        // `dir-link/../m.bin` reads as `m.bin`, but the kernel takes it to `outside/../m.bin`.
        fs::write(f.0.join("m.bin"), "next to the job").unwrap();
        for reference in [
            "file-link",
            "dir-link/s.txt",
            "dir-link/../m.bin",
            "etc-link/passwd",
            "loop",
        ] {
            assert_eq!(f.found("", reference), None, "{reference}");
        }
    }
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_rather_than_opened() {
        let f = Fixture::new("fifo");
        let path = std::ffi::CString::new(f.job().join("fifo").to_str().unwrap()).unwrap();
        // SAFETY: `path` is a valid C string for the duration of the call.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert_eq!(f.found("", "fifo"), None);
    }
    #[test]
    fn unrestricted_references_go_anywhere() {
        let refs = References::unrestricted();
        let path = refs.resolve(Path::new("base"), "../x\\y.png").unwrap();
        assert_eq!(path, Path::new("base/../x/y.png"));
    }
}
