mod fixed;
mod format;
mod import;
mod inspect;
mod repair;
mod split;
mod tmp;
mod translate;
mod vrml;

use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = env::args().skip(1).collect();

    // top-level --help / -h / no args
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        print!(concat!(
            "Usage: objtools <subcommand> [OPTIONS] <file>\n",
            "\n",
            "Subcommands:\n",
            "  import    Convert another mesh format (.wrl) into OBJ\n",
            "  inspect   Extract metadata (object names, vertex counts, materials, bounds)\n",
            "  repair    Repair bad OBJ files (texture paths, case sensitivity, extensions)\n",
            "  split     Partition a large OBJ into per-object output files\n",
            "  translate Translate a Georeferenced OBJ file without loss of precision\n",
            "\n",
            "Run `objtools <subcommand> --help` for subcommand-specific options.\n",
        ));
        return Ok(());
    }

    let subcmd = args.remove(0);
    match subcmd.as_str() {
        "import" => run_import(args),
        "inspect" => run_inspect(args),
        "repair"  => run_repair(args),
        "split"   => run_split(args),
        "translate" => run_translate(args),
        other => {
            eprintln!("Unknown subcommand: {}. Run with --help for usage.", other);
            std::process::exit(1);
        }
    }
}

fn run_import(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut file_path: Option<String> = None;
    let mut output: Option<String> = None;
    let mut tmp_dir: Option<PathBuf> = None;
    let mut keep_tmp = false;
    let mut progress = false;
    let mut it = args.into_iter().peekable();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!(concat!(
                    "Usage: objtools import [OPTIONS] <file.wrl>\n",
                    "\n",
                    "Convert another mesh format into Wavefront OBJ, streaming.\n",
                    "Supported input: .wrl (VRML 97 IndexedFaceSet geometry).\n",
                    "\n",
                    "Coordinates are carried through the fixed-point parser, so a georeferenced\n",
                    "model converts without losing a digit. With -o, a sibling .mtl is written\n",
                    "alongside the .obj for the model's material and texture.\n",
                    "\n",
                    "Options:\n",
                    "  -h, --help        Show this help message and exit\n",
                    "  -o, --output FILE Write the OBJ to FILE (default: stdout, no .mtl)\n",
                    "      --tmp-dir DIR Directory for the index spool files (default: OS temp dir)\n",
                    "      --keep-tmp    Do not delete the spool files after completion\n",
                    "      --progress    Print progress to stderr every 100 MB read\n",
                ));
                return Ok(());
            }
            "-o" | "--output" => {
                output = Some(it.next().ok_or("--output requires a value")?);
            }
            "--tmp-dir" => {
                tmp_dir = Some(PathBuf::from(it.next().ok_or("--tmp-dir requires a value")?));
            }
            "--keep-tmp" => keep_tmp = true,
            "--progress" => progress = true,
            other => {
                if file_path.is_none() { file_path = Some(other.to_string()); }
            }
        }
    }

    let file_path = file_path.ok_or("Missing input file. Run with --help for details.")?;
    let tmp_dir = tmp_dir.unwrap_or_else(std::env::temp_dir);

    import::run(import::ImportOptions { file_path, output, tmp_dir, keep_tmp, progress })
}

fn run_inspect(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut file_path: Option<String> = None;
    let mut json = false;
    let mut compact = false;
    let mut progress = false;
    let mut it = args.into_iter().peekable();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!(concat!(
                    "Usage: objtools inspect [OPTIONS] <file.obj>\n",
                    "\n",
                    "Extract metadata from a Wavefront OBJ file in a single streaming pass.\n",
                    "Outputs the list of objects with their vertex counts, materials and\n",
                    "bounding boxes, plus a whole-file total.\n",
                    "\n",
                    "Options:\n",
                    "  -h, --help      Show this help message and exit\n",
                    "      --json      Output as pretty-printed JSON instead of human-readable text\n",
                    "      --compact   Output as compact single-line JSON (implies --json)\n",
                    "      --progress  Print progress to stderr every 100 MB read\n",
                ));
                return Ok(());
            }
            "--json"    => json = true,
            "--compact" => { json = true; compact = true; }
            "--progress" => progress = true,
            other => {
                if file_path.is_none() { file_path = Some(other.to_string()); }
            }
        }
    }

    let file_path = match file_path {
        Some(p) => p,
        None => {
            eprintln!("Usage: objtools inspect [OPTIONS] <file.obj>\nRun with --help for details.");
            std::process::exit(1);
        }
    };

    inspect::run(inspect::InspectOptions { file_path, json, compact, progress })
}

fn run_split(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut file_path: Option<String> = None;
    let mut output_dir: Option<PathBuf> = None;
    let mut by_material = false;
    let mut tmp_dir: Option<PathBuf> = None;
    let mut keep_tmp = false;
    let mut progress = false;
    let mut it = args.into_iter().peekable();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!(concat!(
                    "Usage: objtools split [OPTIONS] --output-dir <DIR> <file.obj>\n",
                    "\n",
                    "Partition a large OBJ file into per-object output .obj files.\n",
                    "Uses a two-pass algorithm with binary temp files for random vertex access.\n",
                    "\n",
                    "Options:\n",
                    "  --output-dir <DIR>   Directory for output .obj files (required, must exist)\n",
                    "  --by-material        Split by object × material instead of object only\n",
                    "  --tmp-dir <DIR>      Directory for binary temp files (default: OS temp dir)\n",
                    "  --keep-tmp           Do not delete temp files after completion\n",
                    "  --progress           Print progress to stderr (pass 1: every 100 MB; pass 2: per group)\n",
                    "  -h, --help           Show this help and exit\n",
                ));
                return Ok(());
            }
            "--output-dir" => {
                output_dir = Some(PathBuf::from(it.next().ok_or("--output-dir requires a value")?));
            }
            "--by-material" => by_material = true,
            "--tmp-dir" => {
                tmp_dir = Some(PathBuf::from(it.next().ok_or("--tmp-dir requires a value")?));
            }
            "--keep-tmp" => keep_tmp = true,
            "--progress" => progress = true,
            other => {
                if file_path.is_none() { file_path = Some(other.to_string()); }
            }
        }
    }

    let file_path = file_path.ok_or("Missing input file. Run with --help for details.")?;
    let output_dir = output_dir.ok_or("--output-dir is required. Run with --help for details.")?;

    if !output_dir.exists() {
        return Err(format!("Output directory does not exist: {}", output_dir.display()).into());
    }

    let tmp_dir = tmp_dir.unwrap_or_else(std::env::temp_dir);

    split::run(split::SplitOptions {
        file_path,
        output_dir,
        by_material,
        tmp_dir,
        keep_tmp,
        progress,
    })
}



fn run_translate(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut file_path: Option<String> = None;
    let mut output: Option<String> = None;
    let mut progress = false;
    let mut origin_fixed: Option<[String;3]> = None;
    let mut center = false;
    let mut it = args.into_iter().peekable();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!(concat!(
                    "Usage: objtools translate [OPTIONS] <file.obj>\n",
                    "\n",
                    "Translate a georeferenced OBJ so that a chosen origin lands on 0 0 0.\n",
                    "By default the origin is the first vertex in the file.\n",
                    "\n",
                    "Options:\n",
                    "  -h, --help        Show this help message and exit\n",
                    "  -o, --output FILE Write translated OBJ to FILE (default: stdout)\n",
                    "      --progress    Print progress to stderr every 100 MB read\n",
                    "      --origin X,Y,Z Provide origin coordinates (comma-separated) or pass three values\n",
                    "      --center      Use the model's bounding-box center as the origin.\n",
                    "                    Reads the file twice: once to measure, once to write.\n",
                ));
                return Ok(());
            }
            "-o" | "--output" => {
                output = Some(it.next().ok_or("--output requires a value")?);
            }
            "--progress" => progress = true,
            "--center" => center = true,
            "--origin" => {
                let token = it.next().ok_or("--origin requires values")?;
                let parts: Vec<&str> = token.split(',').collect();
                if parts.len() == 3 {
                    origin_fixed = Some([
                        parts[0].to_string(),
                        parts[1].to_string(),
                        parts[2].to_string(),
                    ]);
                } else {
                    // token is X, then next two tokens should be Y and Z
                    let x = token;
                    let y = it.next().ok_or("--origin requires three values")?;
                    let z = it.next().ok_or("--origin requires three values")?;
                    origin_fixed = Some([
                        x.to_string(),
                        y.to_string(),
                        z.to_string(),
                    ]);
                }
            }
            other => {
                if file_path.is_none() { file_path = Some(other.to_string()); }
            }
        }
    }

    let file_path = file_path.ok_or("Missing input file. Run with --help for details.")?;

    if center && origin_fixed.is_some() {
        return Err("--center and --origin are mutually exclusive: --center computes the origin.".into());
    }

    translate::run(translate::TranslateOptions {
        file_path,
        output,
        progress,
        origin: origin_fixed,
        center,
    })
}

fn run_repair(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut file_path: Option<String> = None;
    let mut it = args.into_iter().peekable();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!(concat!(
                    "Usage: objtools repair [OPTIONS] <file.obj>\n",
                    "\n",
                    "Repair bad OBJ files by fixing texture paths, case sensitivity, and file extensions.\n",
                    "\n",
                    "Options:\n",
                    "  -h, --help           Show this help message and exit\n",
                ));
                return Ok(());
            }
            other => {
                if file_path.is_none() { file_path = Some(other.to_string()); }
            }
        }
    }

    let file_path = match file_path {
        Some(p) => p,
        None => {
            eprintln!("Usage: objtools repair [OPTIONS] <file.obj>\nRun with --help for details.");
            std::process::exit(1);
        }
    };

    let opts = repair::RepairOptions {
        file_path,
    };

    let report = repair::run(opts)?;
    
    // Print summary
    if report.success {
        eprintln!("✅ No issues found. File is clean.");
    } else {
        eprintln!("❌ {} issues found. {} fixed, {} remaining.",
            report.issues.len(),
            report.fixed_count,
            report.remaining_issues
        );
    }
    
    Ok(())
}
