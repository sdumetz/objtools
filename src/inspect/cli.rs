//! The `inspect` command line.
use crate::shared::{failure::Usage, Res};
use std::path::PathBuf;
pub fn run(args: impl Iterator<Item = String>) -> Res<()> {
    match parse(args).map_err(Usage)? {
        Some(options) => super::run(options),
        None => Ok(()),
    }
}
/// `None` when there is nothing to run, as after `--help`.
fn parse(args: impl Iterator<Item = String>) -> Res<Option<super::Options>> {
    let mut args = args;
    let mut input = None;
    let mut tmp_dir = None;
    let mut progress = false;
    let mut keep_tmp = false;
    let mut json = false;
    let mut trusted = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                help();
                return Ok(None);
            }
            "--tmp-dir" => {
                tmp_dir = Some(PathBuf::from(
                    args.next().ok_or("--tmp-dir requires a path")?,
                ))
            }
            "--progress" => progress = true,
            "--keep-tmp" => keep_tmp = true,
            "--json" => json = true,
            "--trusted" => trusted = true,
            _ if arg.starts_with('-') => return Err(format!("Unknown option: {arg}").into()),
            _ if input.is_none() => input = Some(PathBuf::from(arg)),
            _ => return Err(format!("Unexpected argument: {arg}").into()),
        }
    }
    Ok(Some(super::Options {
        input: input.ok_or("Missing input file")?,
        tmp_dir,
        keep_tmp,
        progress,
        json,
        trusted,
    }))
}
fn help() {
    println!("Usage: objtools inspect [OPTIONS] <file.gltf|file.glb|file.obj|file.wrl|file.vrml>\n\nReport the vertex and face counts, diffuse texel count and texture occupancy of a\nmodel. OBJ and VRML are imported into a temporary workspace first, so the figures are\nthose of the glTF that `objtools import` would write.\n\n      --tmp-dir DIR  Workspace for the import and coverage maps (default: input directory)\n      --keep-tmp     Keep the workspace for debugging\n      --json         Print the statistics as JSON; the structure may still change\n      --trusted      Let the model refer to files outside its folder\n      --progress     Report progress on stderr\n  -h, --help         Show help");
}
