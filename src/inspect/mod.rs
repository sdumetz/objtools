//! Model statistics, read from glTF. OBJ and VRML are first imported into a private workspace,
//! so every input reports exactly what `objtools import` would produce.
pub mod cli;
mod coverage;
mod measure;
mod report;

use crate::{
    import,
    shared::{disk::Workspace, Res},
};
use std::{
    fs,
    path::{Path, PathBuf},
};
pub struct Options {
    pub input: PathBuf,
    pub tmp_dir: Option<PathBuf>,
    pub keep_tmp: bool,
    pub progress: bool,
    /// Print the report as JSON rather than pretty-printed.
    pub json: bool,
}
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
pub fn run(opts: Options) -> Res<()> {
    let ext = opts
        .input
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(ext.as_str(), "gltf" | "obj" | "wrl" | "vrml") {
        return Err(format!("Unsupported input format: .{ext}").into());
    }
    if !opts.input.is_file() {
        return Err(format!("{}: no such file", opts.input.display()).into());
    }
    // Like import's, this workspace defaults to a real disk rather than a RAM-backed /tmp.
    let tmp_dir = opts
        .tmp_dir
        .unwrap_or_else(|| parent(&opts.input).to_owned());
    let workspace = Workspace::create(&tmp_dir, opts.keep_tmp)?;
    let gltf = if ext == "gltf" {
        opts.input.clone()
    } else {
        let output = workspace.path.join("model.gltf");
        import::run(import::Options {
            input: opts.input.clone(),
            output: output.clone(),
            tmp_dir: workspace.path.clone(),
            center: false,
            z_up: false,
            progress: opts.progress,
            keep_tmp: opts.keep_tmp,
            images: import::images::Options {
                mode: import::images::Mode::Reference,
                ..Default::default()
            },
            quiet: true,
            // Colours change neither the vertices nor the texels counted.
            no_color: true,
        })?;
        output
    };
    let display_base = fs::canonicalize(parent(&opts.input))?;
    let report = measure::measure(&gltf, &display_base, &workspace.path, opts.progress)?;
    // Only the report reaches stdout, so that it can be piped; warnings and progress use stderr.
    if opts.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{report}");
    }
    Ok(())
}
