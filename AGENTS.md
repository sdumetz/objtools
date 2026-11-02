# Goals

`objtools` is a Rust CLI for very large OBJ and VRML meshes, often georeferenced and larger than the machine's RAM plus swap. It has three commands:

- `import` converts OBJ, VRML, glTF or GLB into glTF 2.0 JSON with an external `.bin`, and keeps, copies or converts the textures.
- `inspect` reports vertex, face, diffuse-texel and texture-occupancy statistics. It reads glTF or GLB, and imports OBJ or VRML into a private workspace first.
- `placeholder` builds a rough low-poly stand-in for a model by voxelizing it. It reads glTF written by `import`, and imports OBJ, VRML or GLB into a private workspace first.

The README contains the command reference and inspection statistics. The GitHub wiki has two sections: Conversion (format support, georeferencing, images and material approximations) and How it works (streaming, heap/mmap/disk use and inspection algorithms). Its sources are maintained in the separate `git@github.com:sdumetz/objtools.wiki.git` repository, not in this repository. Keep both in sync with user-visible changes at the appropriate level of detail.

# Constraints

- **Stream, never hold the model.** Input is read line by line or chunk by chunk. Geometry lives in disk spools that are written sequentially and then memory-mapped. Heap use is bounded by one face plus small per-object and per-material records. Any whole-model structure in memory is a bug. `placeholder`'s grids are not one: their size follows the requested resolution, whatever the model's.
- **Workspaces go on a real disk.** Spools default to the output's directory, not the OS temporary directory, which is often RAM-backed (tmpfs).
- **`import` output is faithful.** Don't chunk, cap or duplicate geometry to save memory; use disk-backed structures instead. The only primitive split is at the 32-bit index limit. Coordinates go through the fixed-point path, so `--center` loses no digits. `placeholder` output is approximate by design, but must still overlay its source.
- **Real exports are messy.** Prefer omitting unsupported data with one counted warning over aborting a long import. Reserve errors for input that can't be interpreted, or that would silently give wrong geometry.
- **Interop choices are credited.** When behaviour follows another implementation (three.js, Blender, earcut, obj2gltf, a spec), name it in the code comment or relevant wiki page. That way the choice can be traced, and later swapped.

# Code layout

- `src/main.rs` dispatches to the commands.
- `src/import/` converts OBJ, VRML, glTF and GLB to glTF. `src/inspect/` measures a glTF and reports its statistics, and `src/placeholder/` builds a placeholder from one; both use `import::run` for inputs they don't read directly.
- In each command, `cli.rs` parses arguments and prints help, and `mod.rs` holds `Options` and `run`.
- `src/shared/` holds what more than one command uses: `Res`, workspaces, file mappings and output cleanup, glTF reading, and number formatting. Code used by one command stays in that command's directory.

# Conventions

- Comments and docs describe current behaviour and the reasons for it. They don't describe development history or removed features.
- Every behaviour change comes with tests and a documentation update. Update the README for command options, inspection results and placeholder behaviour, the Conversion wiki section for conversion behaviour, and How it works for implementation details.
- Before committing, run:

  ```sh
  cargo fmt --check
  cargo clippy --all-targets -- -D warnings
  cargo test --locked
  ```

  CI builds with `RUSTFLAGS='-D warnings'` on Linux, macOS and Windows.
- For output changes, run Khronos's glTF validator (`tools/validate_gltf.cjs`; the Development wiki page has the setup). Use `tools/monitor_import.py` for memory measurements on real imports.
