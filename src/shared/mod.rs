//! Code used by more than one command.
pub mod disk;
pub mod failure;
pub mod format;
pub mod gltf;
pub mod references;

pub type Res<T> = Result<T, Box<dyn std::error::Error>>;
