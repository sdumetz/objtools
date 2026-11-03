//! Model statistics, read from glTF or GLB. OBJ and VRML are first imported into a private workspace,
//! so every input reports exactly what `objtools import` would produce.
pub mod cli;
mod coverage;
mod measure;
mod report;

use crate::{
    import,
    shared::{
        disk::{parent, Workspace},
        references::References,
        Res,
    },
};
use std::{fs, path::PathBuf};
pub struct Options {
    pub input: PathBuf,
    pub tmp_dir: Option<PathBuf>,
    pub keep_tmp: bool,
    pub progress: bool,
    /// Print the report as JSON rather than pretty-printed.
    pub json: bool,
    /// Follow file references anywhere, rather than only inside the input's directory.
    pub trusted: bool,
}
pub fn run(opts: Options) -> Res<()> {
    let ext = opts
        .input
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(ext.as_str(), "gltf" | "glb" | "obj" | "wrl" | "vrml") {
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
    let (gltf, references) = if matches!(ext.as_str(), "gltf" | "glb") {
        let references = References::for_input(parent(&opts.input), opts.trusted)?;
        (opts.input.clone(), references)
    } else {
        let output = workspace.path.join("model.gltf");
        import::run(import::Options {
            input: opts.input.clone(),
            output: output.clone(),
            tmp_dir: workspace.path.clone(),
            progress: opts.progress,
            keep_tmp: opts.keep_tmp,
            quiet: true,
            trusted: opts.trusted,
            // Images only need to be where the report can name them: this glTF is never
            // published, and image placement changes none of the figures.
            images: import::images::Options {
                mode: import::images::Mode::Reference,
                ..Default::default()
            },
            // Everything else follows import's defaults, so the figures are those of its output.
            ..Default::default()
        })?;
        // Written by the import above, from references it had already confined. They lead out of
        // the workspace to the source images, so this read has to be exempt.
        (output, References::unrestricted())
    };
    let display_base = fs::canonicalize(parent(&opts.input))?;
    let report = measure::measure(
        &gltf,
        references,
        &display_base,
        &workspace.path,
        opts.progress,
    )?;
    // Only the report reaches stdout, so that it can be piped; warnings and progress use stderr.
    if opts.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{report}");
    }
    Ok(())
}
