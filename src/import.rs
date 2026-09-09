//! `import` — convert a foreign mesh format into Wavefront OBJ.
//!
//! Format is chosen by file extension. Only VRML (`.wrl`) is implemented so far; the dispatch
//! is here so adding the next one does not disturb the CLI.

use std::path::{Path, PathBuf};

pub struct ImportOptions {
    pub file_path: String,
    pub output: Option<String>,
    pub tmp_dir: PathBuf,
    pub keep_tmp: bool,
    pub progress: bool,
}

pub fn run(opts: ImportOptions) -> Result<(), Box<dyn std::error::Error>> {
    let ext = Path::new(&opts.file_path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "wrl" | "vrml" => crate::vrml::convert(&opts),
        "" => Err(format!(
            "{}: no file extension, cannot tell what format this is. \
             objtools import currently reads .wrl (VRML).",
            opts.file_path
        )
        .into()),
        other => Err(format!(
            "unsupported input format `.{}`. objtools import currently reads .wrl (VRML).",
            other
        )
        .into()),
    }
}
