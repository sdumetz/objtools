#![deny(clippy::print_stderr)]

mod import;
mod inspect;
mod shared;

use shared::{failure, Res};
use std::{env, process::ExitCode};
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            crate::say!("Error: {e}");
            ExitCode::from(failure::classify(&*e) as u8)
        }
    }
}
fn run() -> Res<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        None | Some("--help" | "-h") => {
            help();
            Ok(())
        }
        Some("import") => import::cli::run(args),
        Some("inspect") => inspect::cli::run(args),
        Some(other) => {
            Err(format!("Unknown command {other}. Run objtools --help for usage.").into())
        }
    }
}
fn help() {
    println!("Usage: objtools <COMMAND> [OPTIONS] <file>\n\nCommands:\n  import   Convert OBJ, VRML, glTF or GLB to glTF 2.0 + external .bin\n  inspect  Report vertex, face and diffuse texture statistics\n\nRun objtools <COMMAND> --help for its options.");
}
