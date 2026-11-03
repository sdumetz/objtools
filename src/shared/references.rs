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
            // Windows reads the path itself, and `\\\\?\\` only means what it says with backslashes.
            let file = base.join(if cfg!(windows) { reference } else { &written });
            let beside = name_of_absolute(&written).map(|name| base.join(name));
            return Ok(beside
                .filter(|beside| !file.exists() && beside.exists())
                .unwrap_or(file));
        };
        // Decided from the text alone, so the path of a refused reference is never looked up.
        let base = fs::canonicalize(base).map_err(|_| Refused)?;
        if let Some(name) = name_of_absolute(&written) {
            return regular_file_inside(root, &base.join(name));
        }
        if !looks_relative(&written) {
            return Err(Refused);
        }
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
/// The file name to look for beside the referencing file, when the reference is an absolute path.
/// Old exporters write the path a file had on the author's machine where a relative one belongs.
/// Only the name goes any further, so this reaches nothing a relative reference couldn't.
fn name_of_absolute(written: &str) -> Option<&str> {
    let drive = written.split_once(':').filter(|(prefix, _)| {
        prefix.len() == 1 && prefix.starts_with(|c: char| c.is_ascii_alphabetic())
    });
    let path = match drive {
        Some((_, path)) => path,
        None if written.starts_with('/') => written,
        None => return None,
    };
    let name = path.rsplit('/').next()?;
    let usable = !matches!(name, "" | "." | "..")
        && !name.contains('\0')
        && !(cfg!(windows) && name.contains(':'));
    usable.then_some(name)
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
    fn an_absolute_path_is_looked_for_by_name_beside_the_referencing_file() {
        let f = Fixture::new("beside");
        let m = fs::canonicalize(f.job().join("m.bin")).unwrap();
        let a = fs::canonicalize(f.job().join("tex/a.png")).unwrap();
        for reference in [
            "C:\\Users\\bob\\m.bin",
            "C:/Users/bob/m.bin",
            "C:m.bin",
            "/home/bob/m.bin",
            "\\\\server\\share\\m.bin",
            m.to_str().unwrap(),
        ] {
            assert_eq!(f.found("", reference).as_ref(), Some(&m), "{reference}");
        }
        assert_eq!(f.found("tex", "C:\\Users\\bob\\a.png").unwrap(), a);
        // Beside the referencing file only, not in the folders around it.
        assert_eq!(f.found("tex", "C:\\Users\\bob\\m.bin"), None);
        assert_eq!(f.found("", "C:\\Users\\bob\\a.png"), None);
        // Not a name at all, or not a file.
        for reference in ["/", "C:\\", "C:", "/x/..", "/x/.", "/tex", "C:\\x\\a\0b"] {
            assert_eq!(f.found("", reference), None, "{reference:?}");
        }
    }
    #[test]
    fn only_absolute_paths_are_retried() {
        let f = Fixture::new("relative");
        for reference in [
            "sub/dir/m.bin",
            "../outside/m.bin",
            "http://x/m.bin",
            "file:///x/m.bin",
        ] {
            assert_eq!(f.found("", reference), None, "{reference}");
        }
    }
    #[cfg(unix)]
    #[test]
    fn the_retry_is_held_to_the_same_checks() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new("retry");
        symlink(f.0.join("outside/s.txt"), f.job().join("s.txt")).unwrap();
        assert_eq!(f.found("", "C:\\Users\\bob\\s.txt"), None);
    }
    #[test]
    fn absolute_and_remote_references_are_refused() {
        let f = Fixture::new("absolute");
        for reference in [
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
    #[test]
    fn unrestricted_absolute_paths_prefer_what_they_name() {
        let f = Fixture::new("unrestricted");
        let refs = References::unrestricted();
        let job = f.job();
        let outside = fs::canonicalize(f.0.join("outside/s.txt")).unwrap();
        fs::write(job.join("s.txt"), "beside").unwrap();
        let found = refs.resolve(&job, outside.to_str().unwrap()).unwrap();
        assert_eq!(found, outside);
        let found = refs.resolve(&job, "/no/such/folder/s.txt").unwrap();
        assert_eq!(found, job.join("s.txt"));
        // Nothing beside it either: the real path stays, so that the real error is reported.
        let absolute = Path::new("/no/such/folder/other.txt");
        let found = refs.resolve(&job, absolute.to_str().unwrap()).unwrap();
        // Joined, because on Windows a path from the root takes the drive of what it's joined to.
        assert_eq!(found, job.join(absolute));
    }
}
