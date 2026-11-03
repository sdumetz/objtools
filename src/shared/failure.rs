//! What a failure says about its cause, so that a caller can tell a bad file from a bad machine.
use super::Res;
use std::{
    error::Error,
    fmt,
    io::{self, ErrorKind},
};
/// The process exit code of each kind of failure. A panic exits with Rust's 101.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Class {
    /// The input or the command line can't be used, and retrying won't change that.
    Rejected = 1,
    /// The disk is full, or a quota or file-size limit was reached.
    Resource = 3,
    /// Anything else about the machine: permissions, I/O errors, the job's directories.
    Environment = 4,
}
/// Finds the cause of an error along its chain. Errors with no I/O cause come from checking the
/// input, so they are rejections.
pub fn classify(error: &(dyn Error + 'static)) -> Class {
    let mut current = Some(error);
    while let Some(e) = current {
        if let Some(JobDirectory(io)) = e.downcast_ref() {
            return match of_io(io) {
                Class::Resource => Class::Resource,
                _ => Class::Environment,
            };
        }
        if let Some(io) = e.downcast_ref::<io::Error>() {
            return of_io(io);
        }
        // These libraries hide the I/O error they wrap from `source`.
        if let Some(image::ImageError::IoError(io)) = e.downcast_ref() {
            return of_io(io);
        }
        if let Some(png::DecodingError::IoError(io)) = e.downcast_ref() {
            return of_io(io);
        }
        if let Some(kind) = e
            .downcast_ref::<serde_json::Error>()
            .and_then(serde_json::Error::io_error_kind)
        {
            return of_kind(kind);
        }
        current = e.source();
    }
    Class::Rejected
}
fn of_io(error: &io::Error) -> Class {
    match of_kind(error.kind()) {
        // Not from the operating system: a decoder describing the bytes it was given.
        Class::Environment if error.raw_os_error().is_none() => Class::Rejected,
        class => class,
    }
}
fn of_kind(kind: ErrorKind) -> Class {
    match kind {
        ErrorKind::StorageFull | ErrorKind::QuotaExceeded | ErrorKind::FileTooLarge => {
            Class::Resource
        }
        // A reference in the file, or the input path itself, that leads nowhere usable.
        ErrorKind::NotFound
        | ErrorKind::IsADirectory
        | ErrorKind::NotADirectory
        | ErrorKind::InvalidFilename
        | ErrorKind::InvalidInput
        | ErrorKind::InvalidData
        | ErrorKind::UnexpectedEof => Class::Rejected,
        _ => Class::Environment,
    }
}
/// A failure on a directory the caller chose: never the input's fault, even if it is missing.
#[derive(Debug)]
pub struct JobDirectory(pub io::Error);
impl fmt::Display for JobDirectory {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl Error for JobDirectory {}
#[derive(Debug)]
struct Labelled {
    label: String,
    cause: Box<dyn Error>,
}
impl fmt::Display for Labelled {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}: {}", self.label, self.cause)
    }
}
impl Error for Labelled {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&*self.cause)
    }
}
/// Says where an error happened, as `format!` would, but keeps its cause for [`classify`].
pub trait Context<T> {
    fn with_context(self, label: impl FnOnce() -> String) -> Res<T>;
}
impl<T, E: Into<Box<dyn Error>>> Context<T> for Result<T, E> {
    fn with_context(self, label: impl FnOnce() -> String) -> Res<T> {
        self.map_err(|cause| {
            Labelled {
                label: label(),
                cause: cause.into(),
            }
            .into()
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn class_of(error: impl Into<Box<dyn Error>>) -> Class {
        classify(&*error.into())
    }
    fn labelled(error: io::Error) -> Box<dyn Error> {
        let inner = Err::<(), _>(error).with_context(|| "inner".into());
        inner.with_context(|| "outer".into()).unwrap_err()
    }
    #[test]
    fn a_full_disk_and_a_file_size_limit_are_resource_limits() {
        for kind in [
            ErrorKind::StorageFull,
            ErrorKind::QuotaExceeded,
            ErrorKind::FileTooLarge,
        ] {
            assert_eq!(class_of(io::Error::from(kind)), Class::Resource, "{kind}");
        }
    }
    #[test]
    fn the_operating_system_refusing_is_the_environment() {
        // EACCES on Unix, ERROR_ACCESS_DENIED on Windows.
        let denied = io::Error::from_raw_os_error(if cfg!(windows) { 5 } else { 13 });
        assert_eq!(denied.kind(), ErrorKind::PermissionDenied);
        assert_eq!(class_of(denied), Class::Environment);
    }
    #[test]
    fn bad_input_is_rejected() {
        assert_eq!(class_of("Malformed face"), Class::Rejected);
        for kind in [
            ErrorKind::NotFound,
            ErrorKind::IsADirectory,
            ErrorKind::InvalidData,
            ErrorKind::UnexpectedEof,
        ] {
            assert_eq!(class_of(io::Error::from(kind)), Class::Rejected, "{kind}");
        }
    }
    #[test]
    fn an_error_a_decoder_made_up_is_rejected() {
        assert_eq!(class_of(io::Error::other("bad chunk")), Class::Rejected);
    }
    #[test]
    fn a_label_keeps_the_cause_and_reads_like_a_prefix() {
        let error = labelled(io::Error::from(ErrorKind::StorageFull));
        assert_eq!(classify(&*error), Class::Resource);
        assert!(error.to_string().starts_with("outer: inner: "), "{error}");
    }
    #[test]
    fn wrapping_libraries_still_show_the_cause() {
        let full = || io::Error::from(ErrorKind::StorageFull);
        assert_eq!(
            class_of(image::ImageError::IoError(full())),
            Class::Resource
        );
        assert_eq!(
            class_of(png::DecodingError::IoError(full())),
            Class::Resource
        );
        assert_eq!(class_of(serde_json::Error::io(full())), Class::Resource);
        let eof = serde_json::from_slice::<serde_json::Value>(b"{").unwrap_err();
        assert_eq!(class_of(eof), Class::Rejected);
    }
    #[test]
    fn a_job_directory_is_never_the_input() {
        let missing = JobDirectory(io::Error::from(ErrorKind::NotFound));
        assert_eq!(class_of(missing), Class::Environment);
        let full = JobDirectory(io::Error::from(ErrorKind::StorageFull));
        assert_eq!(class_of(full), Class::Resource);
    }
}
