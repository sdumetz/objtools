//! Code used by more than one command.
pub mod diagnostics;
pub mod disk;
pub mod failure;
pub mod format;
pub mod gltf;
pub mod references;

/// `Send`, so that a thread can hand its failure back with its cause intact.
pub type Failure = Box<dyn std::error::Error + Send + Sync>;
pub type Res<T> = Result<T, Failure>;
