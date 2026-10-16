# Goals

`objtools` is a Rust CLI for very large OBJ and VRML meshes, often georeferenced and larger than the machine's RAM plus swap. It has two commands:

- `import` converts OBJ or VRML into glTF 2.0 JSON with an external `.bin`, and keeps, copies or converts the textures.
- `inspect` reports vertex, face, diffuse-texel and texture-occupancy statistics. It reads glTF, and imports OBJ or VRML into a private workspace first.

The README documents the behaviour in full. Keep it in sync with every user-visible change.

# Constraints

- **Stream, never hold the model.** Input is read line by line or chunk by chunk. Geometry lives in disk spools that are written sequentially and then memory-mapped. Heap use is bounded by one face plus small per-object and per-material records. Any whole-model structure in memory is a bug.
- **Workspaces go on a real disk.** Spools default to the output's directory, not the OS temporary directory, which is often RAM-backed (tmpfs).
- **Output is faithful.** Don't chunk, cap or duplicate geometry to save memory; use disk-backed structures instead. The only primitive split is at the 32-bit index limit. Coordinates go through the fixed-point path, so `--center` loses no digits.
- **Real exports are messy.** Prefer omitting unsupported data with one counted warning over aborting a long import. Reserve errors for input that can't be interpreted, or that would silently give wrong geometry.
- **Interop choices are credited.** When behaviour follows another implementation (three.js, Blender, earcut, obj2gltf, a spec), name it in the code comment or README. That way the choice can be traced, and later swapped.

# Conventions

- Comments and docs describe current behaviour and the reasons for it. They don't describe development history or removed features.
- Every behaviour change comes with tests and a README update.
- Before committing, run:

  ```sh
  cargo fmt --check
  cargo clippy --all-targets -- -D warnings
  cargo test --locked
  ```

  CI builds with `RUSTFLAGS='-D warnings'` on Linux, macOS and Windows.
- For output changes, run Khronos's glTF validator (`tools/validate_gltf.cjs`; the README has the setup). Use `tools/monitor_import.py` for memory measurements on real imports.
