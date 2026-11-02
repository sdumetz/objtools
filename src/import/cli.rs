//! The `import` command line.
use super::images;
use crate::shared::Res;
use std::path::{Path, PathBuf};
pub fn run(args: impl Iterator<Item = String>) -> Res<()> {
    let mut args = args;
    let mut input = None;
    let mut output = None;
    let mut tmp_dir = None;
    let mut center = false;
    let mut z_up = false;
    let mut progress = false;
    let mut keep_tmp = false;
    let mut no_color = false;
    let mut images = images::Options::default();
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
            "--center" => center = true,
            "--up-axis" => {
                z_up = match args.next().as_deref() {
                    Some("y") => false,
                    Some("z") => true,
                    _ => return Err("--up-axis must be y or z".into()),
                }
            }
            "--images" => {
                images.mode = images::Mode::parse(
                    &args.next().ok_or("--images requires auto, keep or copy")?,
                )?
            }
            "--convert-images" => {
                images.convert = images::parse_convert(
                    &args
                        .next()
                        .ok_or("--convert-images requires auto, true or false")?,
                )?
            }
            "--image-format" => {
                images.format = images::Format::parse(
                    &args.next().ok_or("--image-format requires jpg or png")?,
                )?
            }
            "--image-quality" => {
                images.quality = args
                    .next()
                    .ok_or("--image-quality requires 1..100")?
                    .parse()?
            }
            _ if arg.starts_with("--convert-images=") => {
                images.convert = images::parse_convert(arg.split_once('=').unwrap().1)?
            }
            _ if arg.starts_with("--images=") => {
                images.mode = images::Mode::parse(arg.split_once('=').unwrap().1)?
            }
            "--progress" => progress = true,
            "--keep-tmp" => keep_tmp = true,
            "--no-color" => no_color = true,
            _ if arg.starts_with('-') => return Err(format!("Unknown option: {arg}").into()),
            _ if input.is_none() => input = Some(PathBuf::from(arg)),
            _ => return Err(format!("Unexpected argument: {arg}").into()),
        }
    }
    if !(1..=100).contains(&images.quality) {
        return Err("--image-quality must be 1..100".into());
    }
    let input = input.ok_or("Missing input file")?;
    let output = output.unwrap_or_else(|| input.with_extension("gltf"));
    // Spools can exceed RAM, and the OS temporary directory is often RAM-backed (tmpfs).
    let tmp_dir = tmp_dir.unwrap_or_else(|| {
        let parent = output.parent().filter(|p| !p.as_os_str().is_empty());
        parent.unwrap_or(Path::new(".")).to_owned()
    });
    super::run(super::Options {
        input,
        output,
        tmp_dir,
        center,
        z_up,
        progress,
        keep_tmp,
        images,
        quiet: false,
        no_color,
    })
}
fn help() {
    println!("Usage: objtools import [OPTIONS] <file.obj|file.wrl|file.vrml>\n\nImport OBJ or VRML IndexedFaceSet geometry into glTF 2.0 + external .bin.\n\n  -o, --output FILE  Destination .gltf (default: input with .gltf extension)\n      --center       Subtract bounding-box center before float32 encoding\n      --up-axis AXIS Source up axis: y (default) or z; output is Y-up\n      --tmp-dir DIR  Disk-backed geometry spools (default: output directory)\n      --keep-tmp     Keep conversion workspace for debugging\n      --images MODE  auto (default), keep source references, or copy images\n      --convert-images BOOL  auto (default), true or false\n      --image-format FORMAT  jpg (default) or png\n      --image-quality N      JPG quality 1..100 (default: 95)\n      --no-color     Drop OBJ vertex colors\n      --progress     Report conversion progress on stderr\n  -h, --help         Show help");
}
