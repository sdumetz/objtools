mod import;
mod inspect;
mod placeholder;
mod shared;

use shared::Res;
use std::env;
fn main() -> Res<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        None | Some("--help" | "-h") => {
            help();
            Ok(())
        }
        Some("import") => import::cli::run(args),
        Some("inspect") => inspect::cli::run(args),
        Some("placeholder") => placeholder::cli::run(args),
        Some(other) => {
            Err(format!("Unknown command {other}. Run objtools --help for usage.").into())
        }
    }
}
fn help() {
    println!("Usage: objtools <COMMAND> [OPTIONS] <file>\n\nCommands:\n  import       Convert OBJ, VRML, glTF or GLB to glTF 2.0 + external .bin\n  inspect      Report vertex, face and diffuse texture statistics\n  placeholder  Build a rough stand-in mesh for a model\n\nRun objtools <COMMAND> --help for its options.");
}
