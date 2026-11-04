//! Import orchestration. Geometry lives on disk, never in a whole-model structure.
pub mod cli;
mod dedup;
mod fixed;
mod from_gltf;
mod gltf;
mod group;
pub mod images;
mod material;
mod obj;
mod smooth;
mod triangulate;
mod vrml;

use self::material::Origin;
use crate::shared::{
    disk::{parent, Workspace},
    failure::{Context, JobDirectory},
    references::References,
    Res,
};
use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};
#[derive(Default)]
pub struct Options {
    pub input: PathBuf,
    pub output: PathBuf,
    pub tmp_dir: PathBuf,
    pub center: bool,
    pub z_up: bool,
    pub progress: bool,
    pub keep_tmp: bool,
    pub images: images::Options,
    /// Only warnings reach stderr; `inspect` imports into a workspace nobody will look at.
    pub quiet: bool,
    /// Drop OBJ vertex colours instead of writing `COLOR_0`/`_COLOR`.
    pub no_color: bool,
    /// Follow file references anywhere, rather than only inside the input's directory.
    pub trusted: bool,
}
pub fn run(mut opts: Options) -> Res<()> {
    opts.images.outside_references = opts.trusted;
    let ext = opts
        .input
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(ext.as_str(), "obj" | "wrl" | "vrml" | "gltf" | "glb") {
        return Err(format!(
            "Unsupported input format: .{}",
            crate::shared::diagnostics::safe(&ext)
        )
        .into());
    }
    // Not `exists`, which follows a link and says no if it leads nowhere.
    let taken = |path: &std::path::Path| fs::symlink_metadata(path).is_ok();
    if taken(&opts.output) || taken(&opts.output.with_extension("bin")) {
        let message = "Output .gltf or .bin already exists; choose a new output path";
        return Err(JobDirectory(io::Error::new(ErrorKind::AlreadyExists, message)).into());
    }
    // With `--tmp-dir` elsewhere, a missing output directory would only show after the import.
    fs::metadata(parent(&opts.output))
        .map_err(JobDirectory)
        .with_context(|| "Output directory".into())?;
    let references = References::for_input(parent(&opts.input), opts.trusted)
        .with_context(|| "Input directory".into())?;
    let workspace = Workspace::create(&opts.tmp_dir, opts.keep_tmp)?;
    workspace.explain(convert(&opts, &ext, references, &workspace.path))
}
fn convert(opts: &Options, ext: &str, references: References, workspace: &Path) -> Res<()> {
    if matches!(ext, "gltf" | "glb") {
        return from_gltf::convert(opts, workspace, references);
    }
    let source = if ext == "obj" {
        opts.input.clone()
    } else {
        let intermediate = workspace.join("source.obj");
        vrml::convert(&opts.input, &intermediate, workspace, opts.progress)?;
        intermediate
    };
    let origin = if ext == "obj" {
        Origin::Model
    } else {
        Origin::GeneratedFrom(&opts.input)
    };
    let scene = obj::spool(
        &source,
        workspace,
        opts.progress,
        &references,
        origin,
        !opts.no_color,
    )?;
    gltf::write(scene, workspace, opts)
}
