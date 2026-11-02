//! Code used by more than one command.
pub mod disk;
pub mod format;

pub type Res<T> = Result<T, Box<dyn std::error::Error>>;
