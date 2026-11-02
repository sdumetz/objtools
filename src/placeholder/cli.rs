//! The `placeholder` command line.
use crate::shared::{disk::parent, Res};
use std::path::PathBuf;
pub fn run(args: impl Iterator<Item = String>) -> Res<()> {
    let mut args = args;
    let mut input = None;
    let mut output = None;
    let mut tmp_dir = None;
    let mut resolution = super::DEFAULT_RESOLUTION;
    let mut threads = None;
    let mut center = false;
    let mut z_up = false;
    let mut progress = false;
    let mut keep_tmp = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                help();
                return Ok(());
            }
            "-o" | "--output" => {
                output = Some(PathBuf::from(
                    args.next().ok_or("--output requires a path")?,
                ))
            }
            "--tmp-dir" => {
                tmp_dir = Some(PathBuf::from(
                    args.next().ok_or("--tmp-dir requires a path")?,
                ))
            }
            "--resolution" => {
                resolution = args
                    .next()
                    .ok_or("--resolution requires a number")?
                    .parse()?
            }
            "--threads" => {
                threads = Some(
                    args.next()
                        .ok_or("--threads requires a number")?
                        .parse::<usize>()?,
                )
            }
            "--center" => center = true,
            "--up-axis" => {
                z_up = match args.next().as_deref() {
                    Some("y") => false,
                    Some("z") => true,
                    _ => return Err("--up-axis must be y or z".into()),
                }
            }
            "--progress" => progress = true,
            "--keep-tmp" => keep_tmp = true,
            _ if arg.starts_with('-') => return Err(format!("Unknown option: {arg}").into()),
            _ if input.is_none() => input = Some(PathBuf::from(arg)),
            _ => return Err(format!("Unexpected argument: {arg}").into()),
        }
    }
    if !super::RESOLUTIONS.contains(&resolution) {
        return Err(format!(
            "--resolution must be {}..{}",
            super::RESOLUTIONS.start(),
            super::RESOLUTIONS.end()
        )
        .into());
    }
    let threads = match threads {
        Some(0) => return Err("--threads must be at least 1".into()),
        Some(n) => n,
        None => std::thread::available_parallelism().map_or(1, usize::from),
    };
    let input = input.ok_or("Missing input file")?;
    let output = output.unwrap_or_else(|| input.with_extension("placeholder.gltf"));
    // A private import's spools can exceed RAM, and the OS temporary directory is often tmpfs.
    let tmp_dir = tmp_dir.unwrap_or_else(|| parent(&output).to_owned());
    super::run(super::Options {
        input,
        output,
        tmp_dir,
        resolution,
        threads,
        center,
        z_up,
        progress,
        keep_tmp,
    })
}
fn help() {
    println!("Usage: objtools placeholder [OPTIONS] <file.gltf|file.glb|file.obj|file.wrl|file.vrml>\n\nBuild a rough, checkered placeholder mesh around a model. A glTF input must have been\nwritten by `objtools import`; OBJ and VRML are imported into a temporary workspace first.\n\n  -o, --output FILE   Destination .gltf (default: input with .placeholder.gltf extension)\n      --resolution N  Cells along the longest axis, 2..128 (default: 16)\n      --threads N     Worker threads (default: all available); each holds its own grid\n      --center        OBJ/VRML/GLB: subtract bounding-box center, as `import --center` does\n      --up-axis AXIS  OBJ/VRML: source up axis, y (default) or z\n      --tmp-dir DIR   Workspace for the import (default: output directory)\n      --keep-tmp      Keep the workspace for debugging\n      --progress      Report progress on stderr\n  -h, --help          Show help");
}
