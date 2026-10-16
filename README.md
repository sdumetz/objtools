# objtools

A Rust command-line importer for very large OBJ and VRML meshes, including georeferenced assets. It writes glTF 2.0 JSON, a sibling binary geometry buffer and keeps, copies or converts referenced textures according to the image policy.

```sh
cargo build --release
objtools import terrain.obj --center -o terrain.gltf
objtools import terrain.wrl --center --up-axis z -o terrain.gltf
objtools inspect terrain.obj
```

The CLI has two commands: `import` converts to glTF, and `inspect` reports model statistics.

## Usage

```text
objtools import [OPTIONS] <file.obj|file.wrl|file.vrml>

-o, --output FILE  Destination .gltf (default: input with .gltf extension)
    --center       Subtract the whole-input bounding-box center
    --up-axis AXIS Source up axis: y (default) or z
    --tmp-dir DIR  Workspace directory (default: the output's directory)
    --keep-tmp     Preserve the workspace for debugging
    --images MODE  auto (default), keep or copy
    --convert-images BOOL  auto (default), true or false
    --image-format FORMAT  jpg (default) or png
    --image-quality N      JPEG quality 1..100 (default 95)
    --no-color     Drop OBJ vertex colors
    --progress     Report reading and conversion progress on stderr
-h, --help         Show help
```

Output directories must already exist. Existing output files are never overwritten. A failed import removes only output files it created. Geometry is assembled in a private workspace before publication, and JSON is written last. Publication of multiple files is not an atomic filesystem transaction.

## Inspect

```text
objtools inspect [OPTIONS] <file.gltf|file.obj|file.wrl|file.vrml>

    --tmp-dir DIR  Workspace for the import and coverage maps (default: the input's directory)
    --keep-tmp     Preserve the workspace for debugging
    --progress     Report progress on stderr
-h, --help         Show help
```

```text
Vertices:           2.25M
Faces:              4.37M
Diffuse texels:     268M in 1 texture
Texture occupancy:  0.018

Texture                                        Size  Occupancy
Arc_de_Triomphe_FULL_u1_v1_diffuse.png  16384x16384  0.018
```

Statistics are always those of a glTF. OBJ and VRML inputs are first imported into a private workspace, exactly as `objtools import` would convert them, and the temporary glTF is inspected and deleted, so `inspect model.obj` and `inspect model.gltf` after an import report the same figures. Import warnings still reach stderr. Texture images are referenced in place, without conversion, so any format the import accepts can be measured. Nothing is written beside the input except the workspace.

- **Vertices** are glTF vertices: one per distinct position/UV/normal combination within a primitive. This is what a renderer uploads. It exceeds the source position count only where glTF requires it: at UV and normal seams, and where a position is used by several materials, since each material is its own primitive. Each mesh counts once, and a POSITION accessor shared by several primitives counts once.
- **Faces** are triangles after triangulation; degenerate faces skipped by import are not counted. Triangle strips and fans count `n - 2`; point and line primitives count none.
- **Diffuse texels** are the width × height of every image used as a material's base-colour texture, each image once.
- **Texture occupancy** is the fraction of those texels a renderer reads when drawing the model, counting texels shared by several triangles once. Padding and unused atlas space are excluded. A texel counts when sampling some point of a UV triangle reads it from the full-resolution level, following the texture's glTF sampler as engines do. With linear filtering (the default, and what `import` writes) that is the 2×2 texels around the point: every texel whose center is less than one texel away on each axis, so charts extend half a texel beyond their edges. With nearest filtering (`magFilter` and `minFilter` both nearest) it is the texel containing the point. UVs go through the texture's `KHR_texture_transform` first, when it has one: offset, rotation, scale and `texCoord` override, as the extension's reference shader applies them. Wrap modes (repeat, mirrored repeat, clamp) apply to these footprints too, so with repeat a chart touching an edge also reads the opposite edge. The global figure is used texels over total texels; each image also reports its own size and occupancy. A triangle that repeats its texture more than 64 times vertically is left out with a warning.

  This follows how GPUs filter textures. OpenGL, OpenGL ES, WebGL and Vulkan define linear filtering at texel coordinate `u` as a blend of texels `⌊u − ½⌋` and `⌊u − ½⌋ + 1` on each axis, whose weights vanish only exactly at a texel center. So a texel is read whenever a sample falls less than one texel from its center. glTF leaves unset filters to the implementation, and common engines such as three.js and Babylon.js default to linear filtering, with mipmaps for minification. Counting only texels whose center a triangle covers would miss texels engines do read: along chart borders, and under triangles smaller than a texel. Lower mipmap levels are not counted: they are derived from the base level, padding included.

Large figures use SI multipliers (k, M, G, T) with three significant digits.

Coverage is one bit per texel in a sparse workspace file per image, memory-mapped like the import spools (32 MiB for a 16k × 16k texture). Buffers are mapped read-only, and image sizes come from file headers without decoding pixels. glTF support covers what objtools writes and the common core: external `.bin` buffers, indexed or non-indexed triangle primitives, float or normalized integer UVs, and `TEXCOORD_n` chosen by `texCoord`. Embedded `data:` URIs, `.glb` and sparse accessors are rejected explicitly.

## Georeferenced coordinates

Position decimals, including scientific notation, are parsed directly into signed 64-bit fixed-point integers with ten fractional digits. Values beyond ten fractional digits round to the nearest fixed-point unit, ties away from zero. The supported range is approximately ±922 million source units.

`--center` measures all declared positions, subtracts the center using widened integer arithmetic, then converts the local result to float32. This avoids first rounding projected world coordinates to float32. The center is rounded to the fixed-point grid; glTF geometry still has float32 precision, so very large local extents can require spatial tiling.

The origin and original bounds are recorded as decimal strings in `extras.objtools`. Restore a source position by undoing the optional axis rotation and adding the stored origin. The origin is not applied back as a scene translation, because that would defeat recentering.

The default assumes Y-up input. `--up-axis z` gives each mesh node a rotation mapping `(x, y, z)` to `(x, z, -y)` in glTF's Y-up frame. Coordinates and origin metadata remain in the source frame. Source units are assumed to be metres; no CRS, reprojection or unit conversion is inferred. For sibling assets that must stay aligned, independently centering each file will give different origins; retain and account for the recorded origins.

## Disk-backed conversion

OBJ is read once into position/UV/normal spools (plus a color spool when positions carry colors) and a sequential face spool. Negative indices are resolved when they appear; positive references are checked against final attribute counts. Once written, the attribute spools are memory-mapped: fixed-width records make every referenced attribute a direct lookup, without a system call per corner and without owning the model on the heap.

Each (object, material, UV layout) becomes exactly one primitive, whatever its size, holding one vertex per distinct `(position, UV, normal)` corner; a color belongs to its position. Faces are first grouped by that key on disk. Objects are already contiguous in the face spool, and when materials interleave inside an object, a counting sort rewrites the spool into a memory-mapped file so each key's faces become contiguous, keeping their file order. The replay then triangulates polygons using ear clipping and deduplicates corners exactly against a memory-mapped table with one 28-byte entry per source position, holding that position's first `(UV, normal)` and its output index. Further combinations at the same position (seams) are chained in a second sparse mapped file. A generation number resets the table between primitives without clearing it. Vertices stream straight into the binary; indices go to a side spool as 32-bit values. When a primitive closes, its vertex count decides the index type: `UNSIGNED_SHORT` up to 65,535 vertices, `UNSIGNED_INT` above. glTF reserves each type's maximum value for primitive restart, so the cut-off is 65,535 and not 65,536. The only split happens when a primitive would exceed 4,294,967,295 vertices, the limit of 32-bit indices; it is reported as a warning. No global mesh optimization is performed: face order and file order of keys are kept.

Heap memory for geometry is bounded by one face, plus one small record per key. Grouping needs disk for a second face spool only when materials interleave. Deduplication uses 28 bytes per source position and 24 bytes per seam corner, in sparse files. Missing normals add one face replay pass and a 24-byte-per-source-position disk spool, mapped read-write. Mapped spool pages are file-backed page cache: the kernel keeps them resident while RAM is free and evicts them back to the workspace files under pressure, so they show up in the process's resident size (RSS) and address space without being able to exhaust memory. JSON metadata grows with the number of objects and (object, material) pairs, and is retained in memory. The exporter supports a single object or primitive larger than RAM; it does not promise constant memory for arbitrarily many objects and materials. OBJ statements, including lines joined by a trailing `\`, and VRML tokens are limited to 1 MiB, VRML nesting to 1,024 levels, and polygons to 4,096 corners; oversized records are rejected explicitly.

VRML is streamed through an IndexedFaceSet reader into a temporary OBJ/MTL, which then follows the same pipeline. This uses extra disk space and I/O but preserves the fixed-point coordinate path. Allow workspace space for the attribute/face spools, generated binary geometry and (for VRML) intermediate OBJ/MTL. Temporary files are removed on success or error unless `--keep-tmp` is set. The workspace defaults to the output's directory rather than the OS temporary directory, which is often RAM-backed (tmpfs) and would defeat the disk backing; keep `--tmp-dir` on a real disk. Random-access conversion benefits from an SSD once the spools outgrow free RAM. A disk that fills up while the smooth-normal spool is written through its mapping ends the process with a bus error (SIGBUS) instead of an error message.

## Materials: Phong to PBR

Phong and PBR cannot be matched exactly. The conversion policy is intentionally explicit:

| Source | glTF mapping |
| --- | --- |
| `Kd` | Linear `baseColorFactor`; default `[0.8, 0.8, 0.8]` |
| `map_Kd` | sRGB base-color texture, multiplied by `Kd`; image policy below, URI escaped. Options: see below |
| `Ns` | Approximate perceptual roughness `(2 / (Ns + 2))^(1/4)` |
| `Ks` | `KHR_materials_specular`, interpreted approximately as normal-incidence reflectance |
| `Ni` | `KHR_materials_ior`; default 1.5 |
| `Pm`, `Pr` | Explicit metalness/roughness overrides |
| `Ke` | Linear emissive factor |
| `d`, `Tr` | Alpha, with `d` taking precedence regardless of property order; alpha below 1 uses `BLEND`; a PNG `map_Kd` also selects `BLEND` when any of its pixels is less than fully opaque, through an alpha channel or `tRNS` |
| `illum 0` | `KHR_materials_unlit` |
| `illum 1` | Specular disabled |
| `illum 4`, `9` (glass) | `KHR_materials_transmission` of `1 − d`; alpha stays 1 and the material `OPAQUE` |
| `illum 6`, `7` (refraction) | `KHR_materials_transmission` of the mean of `Tf` (or `Kt`); `d` still sets alpha |

Metalness defaults to zero: a large or colored `Ks` alone does not establish that a surface is metallic. With IOR 1.5, `Ks` is represented by `specularColorFactor = Ks / 0.04`; other IOR values use their corresponding Fresnel reflectance. IOR 1 produces zero dielectric reflectance. This preserves the approximate specular color/intensity for extension-aware viewers without inventing metalness. Core-only viewers use the metallic-roughness fallback and will render different specular responses. These material extensions are listed in `extensionsUsed`, not required, since core-only viewers still render a close fallback.

An alpha channel alone does not make a texture transparent: baking and photogrammetry tools often write RGBA PNGs whose alpha is 255 everywhere, and blending them would cost sorting artefacts and a transparent render pass for nothing. PNG textures are therefore decoded row by row on a background thread during the geometry conversion, so even a 16k texture adds only a row of memory and no wall time.

Material colors and opacity are clamped to `[0,1]`, shininess to nonnegative values, and IOR must be at least 1. Effective Phong parameters are retained under each material's `extras.sourcePhong`. Scalar colors are treated as linear; exporters that wrote sRGB scalar colors need preprocessing. Ambient `Ka` is omitted because ambient illumination belongs to the viewer's lighting. MTL and glTF both separate two kinds of transparency. `d` dissolves a surface "unlike a real transparent material", which is glTF's alpha as coverage, as in Blender, three.js and obj2gltf. Light passing through a surface, MTL's glass and refraction illumination models, is `KHR_materials_transmission`, listed in `extensionsUsed` only; viewers without it render those materials opaque. glTF tints transmitted light with the base color, so `Tf`'s own color is not kept, and `Tf` under other models is ignored, since some exporters write `Tf 1 1 1` on every material. Raw `d` and `Tf` values remain in `extras.sourcePhong`. A used material that ends up fully transparent (`d 0`, or `Tr 1` without `d`) is kept as written but reported, since it is usually an exporter writing `Tr` as opacity.

VRML material defaults are preserved, including zero specular color, shininess 0.2, transparency and emissive color. VRML shininess is converted to an exponent using a scale of 128. `solid FALSE` becomes `doubleSided`.

`map_Kd` options from the MTL spec are honoured where glTF can express them:

- `-s` and `-o` scale and offset the texture coordinates. They become `KHR_texture_transform`, listed in `extensionsRequired` because a viewer without it would place the texture wrongly, sampling at `s·uv + o` as three.js and Blender do, with the offset converted for glTF's flipped V. The `w` component is ignored.
- `-clamp on` becomes a `CLAMP_TO_EDGE` sampler. That is identical within the 0–1 range, but beyond it glTF stretches the edge texels where MTL leaves the bare material; glTF has no closer mode.
- Non-default `-mm` and `-t` cannot be represented and are ignored with a warning. `-blendu`, `-blendv`, `-cc`, `-texres`, `-boost`, `-bm` and `-imfchan` do not affect a diffuse map and are skipped.
- Any other option is an error, since its argument count, and so where the path starts, is unknown.

Unsupported MTL maps/properties warn explicitly. Missing textures and image formats that cannot be decoded (or require disabled conversion) are errors. Faces without UVs cannot sample a texture, and glTF forbids their primitive from referencing one, so they use a copy of their material without the texture, named `<material>-untextured`, as obj2gltf does; a warning counts them. Separate opacity/specular/bump maps are not baked into PBR textures. Both OBJ and VRML UVs have V flipped for glTF's texture convention.

Policy references: [KHR_materials_specular](https://github.com/KhronosGroup/glTF/tree/main/extensions/2.0/Khronos/KHR_materials_specular), [VRML lighting model](https://www.web3d.org/documents/specifications/14772/V2.0/part1/concepts.html).

## Image policy

`--images auto` keeps original image references when output and input are in the same folder and the image needs no conversion. It copies images otherwise. `--images keep` explicitly references original files using relative, escaped URIs, even across folders; `--images copy` explicitly copies images. Sources are never modified. Conversion always writes a new image in the output folder, even with `--images keep`.

`--convert-images auto` does not convert JPG/JPEG or PNG; all other source formats require conversion. `--convert-images true` converts every referenced image. `--convert-images false` forbids conversion: encountering a non-JPG/PNG image is an error and aborts the import. Both `--convert-images=false` and `--convert-images false` are accepted.

Conversion defaults to `--image-format jpg --image-quality 95`. PNG conversion is lossless and keeps alpha; quality only applies to JPEG. JPEG composites transparent pixels onto white and reports this explicitly. Choosing a format does not itself force conversion of already-supported files: use `--convert-images true` for that.

Supported conversion inputs include BMP, TGA, TIFF, WebP, GIF, PNM, HDR, DDS, ICO, QOI and EXR, in addition to JPG/PNG. Unsupported codecs, missing files and corrupt input images are errors. Conversion decodes one image at a time with the image library's allocation limits (default 512 MiB); its memory usage depends on image dimensions, separately from the bounded geometry pipeline. Unconverted JPG/PNG images use header checks and byte copies, without decoding all pixels. Animation is reduced to the decoded image/frame.

```sh
objtools import mesh.obj --images keep -o elsewhere/mesh.gltf
objtools import mesh.obj --convert-images true --image-format jpg --image-quality 95
objtools import mesh.obj --convert-images true --image-format png
```

## Geometry coverage and limitations

OBJ objects become named meshes/nodes. Each material used by an object is one primitive, and faces with and without UVs in the same material are two. Every primitive owns its buffer views; no vertex buffer is shared between meshes. Empty objects are omitted. Many exporters write groups (`g`) and no objects, so until a file's first `o` each `g` starts an object named after the group; groups inside objects are not represented. A repeated `o` or `g` name starts a new object rather than reopening the earlier one, as in obj2gltf, which also maps groups to meshes.

Valid source normals are preserved and normalized. Zero-length source normals are repaired with generated smooth normals and reported. Missing normals default to smooth shading: triangle normals are accumulated with area weights by source position index in a disk-backed spool, then normalized. Smoothing groups only switch flat shading, as in three.js: faces after `s off` or `s 0` get their own face normal, are not shared with other faces, and do not weigh on their neighbours' smooth normals. A group number or a bare `s` smooths faces together again; different numbers are not kept apart, as in three.js and Blender, so no hard edges appear between groups. Files without `s`, such as most scans, stay smooth (Blender would shade them flat). VRML crease angles do not disable the smooth default. Smoothing crosses material and UV boundaries for shared position indices; separate position indices are not welded. Opposing faces whose accumulated normals cancel fall back to their polygon normal; such corners are never shared, since each carries its own face's normal. Faces mixing present and missing UVs/normals are rejected. OBJ and MTL keywords match whatever their case, as in obj2gltf and three.js. A trailing `\` continues any OBJ statement on the next line, as in Blender. Comments are stripped first, so a comment line never continues. Unsupported OBJ records warn and are omitted. A face with fewer than three corners is really a line or a point, so it is omitted the same way and counted on stderr, after its indices are checked.

Triangulation never aborts an import (`src/triangulate.rs`, one function per rule, each crediting its source):

- A corner at the same position as the previous one is dropped, so `f 1 2 3 3` is a triangle. This follows earcut's point filtering.
- A face's plane is that of its Newell normal. When that normal vanishes, as with a bowtie whose two halves cancel, the plane comes from its largest fan triangle.
- Ears are found with earcut's test, the library obj2gltf uses: only reflex or flat corners can block an ear, and a corner at the position of the ear's first corner does not. Keyhole polygons, a hole joined to the outline by a doubled edge, therefore triangulate exactly.
- When no ear exists, the face is self-intersecting or otherwise not simple. A convex or flat corner is clipped anyway, following Blender's polygon filling and Held's FIST, so the face is covered rather than dropped. These faces are counted in a warning.
- Faces and triangles of zero area are skipped, with a count on stderr.

Per-vertex colors written after the position (`v x y z r g b`, as exported by RealityCapture, Metashape or MeshLab) become a float `COLOR_0` attribute. They are read as sRGB display values in `[0,1]` and converted to linear, as glTF requires; values outside that range are clamped. Positions without a color in a file that has some are white. glTF multiplies `COLOR_0` into the base color, which would tint a `map_Kd` texture a second time. Textured faces therefore keep their colors as `_COLOR` instead, in the same linear float format: glTF reserves a leading underscore for application-specific attributes, which renderers ignore, so the data survives without changing the rendering. A warning counts those faces. Untextured faces of the same file, including faces without UVs whose material has a texture, use `COLOR_0`. `--no-color` drops vertex colors altogether.

Vertex components with no glTF equivalent are omitted with one warning per kind instead of failing the import: a color alpha (`v x y z r g b a`), a rational weight other than 1 (`v x y z w`, which is not applied as a divisor), any other extra `v` component, and texture coordinates beyond `u v`.

VRML coverage is IndexedFaceSet geometry with inline Coordinate, TextureCoordinate, Normal, Material and ImageTexture nodes. Index arrays may arrive in any order. Non-identity transforms, USE references, non-IndexedFaceSet primitive geometry, conditional nodes, `ccw FALSE` and `normalPerVertex FALSE` are rejected rather than silently producing misplaced geometry. VRML per-vertex colors warn and are omitted; texture transforms, generated UVs and animation are not implemented. Non-default fields beyond this subset are not a complete VRML scene conversion.

## Development

```sh
cargo fmt --check
RUSTFLAGS='-D warnings' cargo build --locked
cargo test --locked
cargo clippy --all-targets -- -D warnings
```

Tests cover coordinate precision, scientific notation, negative indices, UV seams, material parameters, mesh ranges, VRML defaults, concave polygons, one primitive per material under shuffled and interleaved face orders, 16/32-bit index selection, output cleanup, inspect reports matching across OBJ and imported glTF, and texel coverage against a brute-force overlap test. The glTF writer uses Serde for metadata and a small binary writer, with no mesh-owning library or C/C++ dependency. CI builds and tests on Linux, macOS and Windows.

Optional format validation with Khronos's validator, for models that fit in memory (it loads the whole `.bin`):

```sh
npm install --prefix /tmp/objtools-validator gltf-validator
NODE_PATH=/tmp/objtools-validator/node_modules node tools/validate_gltf.cjs terrain.gltf
```

For a real import with Linux process-memory monitoring:

```sh
python3 tools/monitor_import.py input.obj destination/model.gltf --center --progress --tmp-dir destination
```

The monitor writes import logs, one-second RSS samples and a peak-memory report beside the model. RSS includes the mapped spools (evictable page cache); the anonymous figure is the importer's own heap.
