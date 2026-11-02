//! The `inspect` command line.
use crate::shared::Res;
use std::path::PathBuf;
pub fn run(args: impl Iterator<Item = String>) -> Res<()> {
    let mut args = args;
    let mut input = None;
    let mut tmp_dir = None;
    let mut progress = false;
    let mut keep_tmp = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                help();
                return Ok(());
            }
            "--tmp-dir" => {
                tmp_dir = Some(PathBuf::from(
                    args.next().ok_or("--tmp-dir requires a path")?,
                ))
            }
            "--progress" => progress = true,
            "--keep-tmp" => keep_tmp = true,
            _ if arg.starts_with('-') => return Err(format!("Unknown option: {arg}").into()),
            _ if input.is_none() => input = Some(PathBuf::from(arg)),
            _ => return Err(format!("Unexpected argument: {arg}").into()),
        }
    }
    super::run(super::Options {
        input: input.ok_or("Missing input file")?,
        tmp_dir,
        keep_tmp,
        progress,
    })
}
fn help() {
    println!("Usage: objtools inspect [OPTIONS] <file.gltf|file.obj|file.wrl|file.vrml>\n\nReport the vertex and face counts, diffuse texel count and texture occupancy of a\nmodel. OBJ and VRML are imported into a temporary workspace first, so the figures are\nthose of the glTF that `objtools import` would write.\n\n      --tmp-dir DIR  Workspace for the import and coverage maps (default: input directory)\n      --keep-tmp     Keep the workspace for debugging\n      --progress     Report progress on stderr\n  -h, --help         Show help");
}
