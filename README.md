# objtools

Command-line tools for **very large** Wavefront OBJ files — files that are several times
bigger than the machine's RAM and swap combined.

Every subcommand reads its input as a stream, one line at a time, and never holds the mesh
in memory. A 400 GB OBJ is processed with the same memory footprint as a 400 kB one.

```
$ objtools inspect model.obj
┌─ 2 objects
├── 「Cube」
│     ├─ 4 vertices
│     └─ 2 materials: Wood, Metal
└── 「Sphere」
      ├─ 3 vertices
      └─ 1 material: Glass
```

## Goals

- **Never OOM.** Whatever the input size, the tool must complete. No full-mesh buffering, no
  memory-mapping the whole file, no "load then query".
- **Metadata first.** Answer *what is in this file?* — object names, per-object vertex counts,
  per-object material lists — in a single pass over the bytes.
- **Useful transforms on the way.** Splitting a monolithic OBJ into per-object files and
  re-centering a georeferenced mesh are the two operations that otherwise force you to open the
  file in a DCC tool that cannot load it.
- **No precision loss.** Georeferenced meshes carry coordinates in the millions of metres with
  sub-millimetre detail. Round-tripping those through `f32`, or even naively through `f64`,
  visibly destroys the model.

## Installation

Prebuilt binaries for Linux, macOS (Intel and Apple Silicon) and Windows are attached to each
release — download the archive for your platform and put `objtools` on your `PATH`.

To build from source you need a recent stable Rust toolchain:

```sh
cargo build --release
# binary at ./target/release/objtools
```

## Usage

```
objtools <subcommand> [OPTIONS] <file.obj>

Subcommands:
  inspect    Extract metadata (object names, vertex counts, materials)
  split      Partition a large OBJ into per-object output files
  translate  Translate a georeferenced OBJ file without loss of precision
```

Every subcommand accepts `--help`, and `--progress` to report advancement on stderr while
chewing through a large file.

### `inspect`

Single streaming pass. Reports every `o` group with its vertex count and the materials it uses.

```sh
objtools inspect model.obj             # tree view
objtools inspect --json model.obj      # pretty-printed JSON
objtools inspect --compact model.obj   # single-line JSON, for piping into jq
objtools inspect --progress model.obj  # progress on stderr every 100 MB
```

```json
{
  "total_objects": 2,
  "objects": [
    { "name": "Cube",   "vertex_count": 4, "materials": ["Wood", "Metal"] },
    { "name": "Sphere", "vertex_count": 3, "materials": ["Glass"] }
  ]
}
```

Vertices are attributed to the `o` group they are declared under. A file with geometry before
any `o` line reports it under the name `(default)`.

### `split`

Writes one self-contained `.obj` per object into an existing output directory. Each output file
carries the original `mtllib` declarations, only the vertices/texcoords/normals its faces
actually reference, and face indices renumbered accordingly.

```sh
mkdir -p parts
objtools split --output-dir parts model.obj

objtools split --output-dir parts --by-material model.obj   # one file per object × material
objtools split --output-dir parts --tmp-dir /mnt/big model.obj
```

| Option | Effect |
| --- | --- |
| `--output-dir <DIR>` | Destination for the `.obj` files. Required, must already exist. |
| `--by-material` | Split on object × material instead of object only. |
| `--tmp-dir <DIR>` | Where to put the intermediate binary files (default: OS temp dir). Point this at a disk with room for roughly the size of the input. |
| `--keep-tmp` | Keep the intermediate files, for debugging. |
| `--progress` | Report pass 1 every 100 MB, then one line per group in pass 2. |

Output names are derived from the object name with path-hostile characters replaced by `_`;
collisions get a `_2`, `_3`, … suffix rather than overwriting.

### `translate`

Subtracts a fixed origin from every vertex, so a mesh authored in projected world coordinates
(Lambert-93, UTM…) ends up near `0 0 0` where renderers keep their precision.

```sh
objtools translate model.obj -o centered.obj                     # origin = first vertex
objtools translate --origin 651000,6862000,120 model.obj -o centered.obj
objtools translate --origin 651000 6862000 120 model.obj         # to stdout
```

With no `--origin`, the first `v` line encountered becomes the origin and is echoed on stderr,
so the same value can be reused later — for instance to translate sibling files by exactly the
same amount. Non-vertex lines are passed through untouched.

## Design

### Streaming, always

`inspect` and `translate` are pure single-pass streamers: a 64 kB `BufReader`, a line at a time,
constant memory. `inspect` retains only the accumulated per-object records — a few hundred bytes
per object, so even a file with a million objects stays well under a gigabyte. `translate` retains
nothing at all beyond the origin.

Nothing ever calls `read_to_string`, and nothing memory-maps the input. That is the whole trick,
and it is the constraint every future subcommand has to respect.

### `split`: two passes with binary side files

Splitting cannot be done in one pass. Faces reference vertices by global index and OBJ allows a
face to point back at a vertex declared arbitrarily far earlier in the file, so writing out
"object N" requires random access to vertex data that the stream has long since passed.

Rather than keep vertices in memory, pass 1 spools them to fixed-width binary side files:

```
Pass 1 (streaming read of the input)
  v  ──▶ verts.bin       24 bytes per record (3 × f64)
  vt ──▶ texcoords.bin   16 bytes per record (2 × f64)
  vn ──▶ normals.bin     24 bytes per record (3 × f64)
  f  ──▶ faces__<object>.bin   one file per output group
  usemtl ──▶ sentinel record inside the group's face file

Pass 2 (per group)
  a. scan the group's face file, collect + sort + dedup the indices it references
  b. seek into verts.bin / texcoords.bin / normals.bin at index × record size
  c. re-scan the face file, emit faces with indices remapped via binary search
```

Fixed-width records are what make step (b) an `O(1)` seek instead of a scan: vertex *n* always
lives at byte `(n - 1) × 24`. Face records are `1 + 12 × N` bytes (a vertex count, then
`(v, vt, vn)` as three `u32`s, `0` meaning absent), and negative OBJ indices are resolved against
the running counts during pass 1, while those counts are still correct.

Material changes inside a group are preserved by writing a sentinel record (`N = 0` followed by a
material id) into the face stream, so `usemtl` lines come back out in the right places rather than
being flattened to one material per file.

The only unbounded structure is the set of groups, and even there open file handles are capped at
`MAX_OPEN_FACE_FILES` (400): writers are closed and reopened in append mode as needed, so a file
with tens of thousands of objects does not exhaust the process's descriptor limit.

Cost: temp files roughly the size of the input geometry, and two reads instead of one. Temp files
are removed by a `Drop` guard, including on error, unless `--keep-tmp` is given.

### `translate`: fixed-point arithmetic

Georeferenced coordinates are the pathological case for floating point. A value like
`651234.5678901234` uses most of an `f64`'s 15–16 significant digits on the part that is about to
be subtracted away, and reformatting it through Rust's float printer does not necessarily give back
the digits that were in the file.

So `translate` never converts to `f64` at all (except for exponent-notation inputs, which fall back
to a float parse). Decimal strings are parsed **by hand** into `i64` fixed-point with 10 fractional
digits — a scale of 0.1 nm, with an `i64` range of ±922 million metres, comfortably covering any
projected coordinate system. Subtraction is then exact integer arithmetic, and the result is
formatted back to decimal with trailing zeros trimmed, so `1000001 - 1000000` prints as `1` and not
`0.9999999999`.

### Why Rust

Predictable memory (no GC pauses on multi-hour runs), `BufReader`/`BufWriter` that make the
streaming discipline the natural way to write the code, cheap exact integer arithmetic, and a
single static binary per platform with no runtime to install on the machine doing the processing.

## Development

```sh
cargo build           # debug build
cargo test            # unit tests
cargo build --release # optimised: LTO, one codegen unit
```

CI (`.github/workflows/ci.yml`) builds and tests on Linux, macOS and Windows for every push and
pull request, then cross-builds release binaries for four targets:

| Platform | Target |
| --- | --- |
| Linux x86-64 | `x86_64-unknown-linux-gnu` |
| macOS Apple Silicon | `aarch64-apple-darwin` |
| macOS Intel | `x86_64-apple-darwin` |
| Windows x86-64 | `x86_64-pc-windows-msvc` |

CI runs with `RUSTFLAGS=-D warnings`, so a compiler warning fails the build — keep the tree
warning-free, or check locally with `RUSTFLAGS="-D warnings" cargo build`.

Pushing a `v*` tag (e.g. `v0.2.0`) runs the same pipeline and, on success, creates a GitHub release
with the archives for all four targets attached:

```sh
git tag v0.2.0
git push origin v0.2.0
```
