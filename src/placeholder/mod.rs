//! A rough placeholder mesh, built by voxelizing a model's triangles in one streamed pass over
//! its mapped glTF. Output size depends on the resolution, never on the model's face count.
pub mod cli;
mod model;
mod output;
mod surface;
mod voxel;

use crate::{
    import,
    shared::{disk::Workspace, gltf::Gltf, Res},
};
use std::{ops::RangeInclusive, path::PathBuf};
pub const DEFAULT_RESOLUTION: usize = 16;
/// Below 2, every surface vertex collapses onto one cell's mean. Above 128, per-thread grids
/// pass 70 MiB each, for detail a placeholder doesn't need.
pub const RESOLUTIONS: RangeInclusive<usize> = 2..=128;
pub struct Options {
    pub input: PathBuf,
    pub output: PathBuf,
    pub tmp_dir: PathBuf,
    pub resolution: usize,
    pub threads: usize,
    pub center: bool,
    pub z_up: bool,
    pub progress: bool,
    pub keep_tmp: bool,
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
    if ext == "gltf" && (opts.center || opts.z_up) {
        return Err("--center and --up-axis only apply to inputs that get imported: a glTF already has its coordinates, and the placeholder shares them".into());
    }
    if opts.output.extension().and_then(|s| s.to_str()) != Some("gltf") {
        return Err("Output must have a .gltf extension".into());
    }
    if opts.output.exists() || opts.output.with_extension("bin").exists() {
        return Err("Output .gltf or .bin already exists; choose a new output path".into());
    }
    // Holds the import, and whatever `Gltf::open` decodes from `data:` URIs.
    let space = Workspace::create(&opts.tmp_dir, opts.keep_tmp)?;
    let source = if ext == "gltf" {
        opts.input.clone()
    } else {
        let imported = space.path.join("model.gltf");
        import::run(import::Options {
            input: opts.input.clone(),
            output: imported.clone(),
            tmp_dir: space.path.clone(),
            center: opts.center,
            z_up: opts.z_up,
            progress: opts.progress,
            keep_tmp: opts.keep_tmp,
            quiet: true,
            // The placeholder uses neither images nor colours.
            images: import::images::Options {
                mode: import::images::Mode::Reference,
                ..Default::default()
            },
            no_color: true,
        })?;
        imported
    };
    let gltf = Gltf::open(&source, &space.path)?;
    let model = model::Model::read(&gltf).map_err(|e| format!("{}: {e}", opts.input.display()))?;
    let grid = voxel::build(&model, opts.resolution, opts.threads, opts.progress)?;
    let mesh = surface::extract(&grid)?;
    output::write(&mesh, &gltf.root, &model, &opts)
}
