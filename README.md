# objtools

Command-line tools to manipulate Wavefront OBJ files. Designed to accomodate very large file on human-sized computers. It can be used to import a mesh from another format, split an OBj file into its constituent models, center georeferenced coordinates or just analyze the content of a file.

Every subcommand reads its input as a stream, one line at a time, and never holds more data than it has to to achieve low resource usage and good speed.

```
$ objtools inspect model.obj
┌─ 2 objects
├── 「Cube」
│     ├─ 4 vertices
│     ├─ 2 materials: Wood, Metal
│     └─ bounds 0 0 0 → 1 1 0
└── 「Sphere」
      ├─ 3 vertices
      ├─ 1 material: Glass
      └─ bounds 2 0 0 → 3 1 0

   total   7 vertices in 2 objects
   bounds  0 0 0 → 3 1 0
   size    3 × 1 × 0
   center  1.5 0.5 0
```

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
objtools <subcommand> [OPTIONS] <file>

Subcommands:
  import     Convert another mesh format (.wrl) into OBJ
  inspect    Extract metadata (object names, vertex counts, materials, bounds)
  split      Partition a large OBJ into per-object output files
  translate  Translate a georeferenced OBJ file without loss of precision
```

Every subcommand accepts `--help`, and `--progress` to report advancement on stderr while
chewing through a large file.

### `import`

Converts a mesh in another format into OBJ. The input format is chosen by file extension.

Supports VRML (`.wrl`) files, more should be added when needed.

```sh
objtools import model.wrl -o model.obj    # also writes model.mtl
objtools import model.wrl                 # OBJ on stdout, no .mtl
objtools import --progress model.wrl -o model.obj
```

| Option | Effect |
| --- | --- |
| `-o, --output FILE` | Write the OBJ to `FILE`, and a sibling `.mtl` for the material. Without it the OBJ goes to stdout and no `.mtl` is produced. |
| `--tmp-dir <DIR>` | Where to put the index spool files (default: OS temp dir). |
| `--keep-tmp` | Keep the spool files, for debugging. |
| `--progress` | Report progress on stderr every 100 MB read. |

Each VRML `Shape` becomes an OBJ object, named after its `DEF` name where it has one and
`shape_1`, `shape_2`, … otherwise. `Material` and `ImageTexture` become a `usemtl`/`newmtl` pair
carrying `Kd`, `Ks`, `Ns`, `d` and `map_Kd`. Coordinates go through the same fixed-point path as
`translate`, so a georeferenced model converts **without losing a digit**.

What is read: `IndexedFaceSet` geometry — `coord`/`Coordinate`, `texCoord`/`TextureCoordinate`,
`normal`/`Normal` and the `coordIndex` / `texCoordIndex` / `normalIndex` arrays, in whatever order
the file declares them. Per VRML's rules an absent `texCoordIndex` falls back to `coordIndex`.
Polygons are passed through as n-gons rather than triangulated. The VRML 1.0 spellings
`Coordinate3` and `TextureCoordinate2` are accepted.

What is **not** read. Each of these warns on stderr rather than failing, so a conversion never
silently loses something without saying so:

- **Per-vertex colours** (`Color` nodes) are dropped — OBJ has no standard way to carry them.
- **`Transform` translation/scale** is not applied; the output keeps the raw coordinates. A
  non-identity transform produces a warning so it cannot pass unnoticed.
- `Switch`, `LOD`, `Billboard`, `Extrusion`, `ElevationGrid` and the other non-`IndexedFaceSet`
  geometry nodes are skipped, as is `USE` node reuse.

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
  "total_vertices": 7,
  "bounding_box": {
    "min":    ["0", "0", "0"],
    "max":    ["3", "1", "0"],
    "size":   ["3", "1", "0"],
    "center": ["1.5", "0.5", "0"]
  },
  "objects": [
    {
      "name": "Cube",
      "vertex_count": 4,
      "materials": ["Wood", "Metal"],
      "bounding_box": { "min": ["0", "0", "0"], "max": ["1", "1", "0"], "…": [] }
    }
  ]
}
```

Vertices are attributed to the `o` group they are declared under. A file with geometry before
any `o` line reports it under the name `(default)`.

Bounding-box coordinates are emitted as decimal **strings**, not JSON numbers. A georeferenced
coordinate carries more significant digits than a JSON number survives in most parsers, and
quietly rounding them here would defeat the point of the fixed-point pipeline described below.
An object with no vertices reports no bounds rather than a box collapsed on the origin.

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

Subtracts a chosen origin from every vertex, so a mesh authored in projected world coordinates
(Lambert-93, UTM…) ends up near `0 0 0` where renderers keep their precision.

```sh
objtools translate --center model.obj -o centered.obj            # origin = bounding-box center
objtools translate --origin 651000,6862000,120 model.obj -o centered.obj
objtools translate --origin 651000 6862000 120 model.obj         # to stdout
objtools translate model.obj -o centered.obj                     # origin = first vertex
```

There are three ways to pick the origin:

| Mode | Origin | Passes |
| --- | --- | --- |
| `--center` | Center of the model's bounding box. | 2 |
| `--origin X,Y,Z` | Exactly what you give it. | 1 |
| *(default)* | The first `v` line in the file. | 1 |

`--center` is usually what you want: it puts the model's actual middle on the origin, which the
first vertex only does by accident. It costs a first streaming pass to measure the bounding box —
still constant memory, just twice the reading. `--center` and `--origin` are mutually exclusive,
and centering a file with no vertices is an error rather than a silent copy.

Whichever mode is used, the origin is echoed on stderr, so the same value can be fed back through
`--origin` later — for instance to translate sibling files by exactly the same amount, or to
translate the geometry of a scene one object at a time. Non-vertex lines are passed through
untouched.

## Design

### Streaming, always

`inspect` and `translate` are line-at-a-time streamers over a 64 kB `BufReader`, in constant
memory. `inspect` retains only the accumulated per-object records — a few hundred bytes per
object, so even a file with a million objects stays well under a gigabyte. `translate` retains
nothing at all beyond the origin.

A bounding box is six integers, so measuring one costs nothing in memory: `inspect` keeps one per
object plus a running total, and `translate --center` keeps exactly one for its first pass. What it
does cost is *parsing* — the default `inspect` has to turn every coordinate into a number rather
than just counting `v` lines, which is why `parse_decimal_fixed` is written to be allocation-free
(see below). On a 393 MB / 2.2 M-vertex file the bounding box adds roughly 30% to the wall time.

Nothing ever calls `read_to_string`, and nothing memory-maps the input. That is the whole trick,
and it is the constraint every future subcommand has to respect.

### `import`: spooling the index arrays

VRML stores a face's position indices and its texture-coordinate indices in two separate arrays
(`coordIndex`, `texCoordIndex`) that pair up positionally, and it is free to declare them in
either order — the sample file this was built against puts `texCoordIndex` first. OBJ wants the
two woven together on a single `f` line, so the converter cannot emit a face until it has seen
every index array of that `Shape`.

Keeping them in memory is exactly what this project refuses to do, so each index array is spooled
to a fixed-width binary temp file as it streams past (little-endian `i32`, VRML's `-1` face
terminators kept inline). At the end of each `Shape` the spools are replayed in lockstep and the
`f` lines are written. Vertices, texture coordinates and normals need no spool at all: they are
written to the OBJ as they are parsed, which also happens to put them ahead of the faces that
reference them, exactly where OBJ wants them.

The reader underneath is a byte-level scanner rather than a line reader — VRML arrays run to
millions of tokens spread arbitrarily across lines, so there are no line boundaries worth
respecting. Converting a 228 MB `.wrl` (1.05 M vertices, 2.10 M faces) runs in about ten seconds
and completes unchanged under a 64 MB address-space cap.

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

So coordinates are never converted to `f64` at all (except for exponent-notation inputs, which fall
back to a float parse). Decimal strings are parsed **by hand** into `i64` fixed-point with 10
fractional digits — a scale of 0.1 nm, with an `i64` range of ±922 million metres, comfortably
covering any projected coordinate system. Subtraction is then exact integer arithmetic, and the
result is formatted back to decimal with trailing zeros trimmed, so `1000001 - 1000000` prints as
`1` and not `0.9999999999`.

All three of `inspect`, `translate` and `import` share this path, which is why a `.wrl` can be
converted, centered and measured without any stage rounding the coordinates the previous one
produced. That parser (`src/fixed.rs`) runs three times per vertex, so it walks the bytes of the number
directly and allocates nothing. The same module holds `BBox`, which is why `inspect`'s bounds,
`translate --center`'s origin and the translation itself all agree to the last digit: the midpoint
of the box is computed as an exact integer, widened to `i128` only so the intermediate sum cannot
overflow.

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
