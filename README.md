# objtools

A Rust command-line importer for very large OBJ and VRML meshes, including georeferenced assets. It writes glTF 2.0 JSON, a sibling binary geometry buffer and keeps, copies or converts referenced textures according to the image policy.

```sh
cargo build --release
objtools import terrain.obj --center -o terrain.gltf
objtools import terrain.wrl --center --up-axis z -o terrain.gltf
```

The CLI has one command, `import`. The previous `inspect`, `translate`, `repair` and `split` commands have been removed; VRML-to-OBJ is now an internal adapter.

## Usage

```text
objtools import [OPTIONS] <file.obj|file.wrl|file.vrml>

-o, --output FILE  Destination .gltf (default: input with .gltf extension)
    --center       Subtract the whole-input bounding-box center
    --up-axis AXIS Source up axis: y (default) or z
    --tmp-dir DIR  Workspace directory (default: OS temporary directory)
    --keep-tmp     Preserve the workspace for debugging
    --images MODE  auto (default), keep or copy
    --convert-images BOOL  auto (default), true or false
    --image-format FORMAT  jpg (default) or png
    --image-quality N      JPEG quality 1..100 (default 95)
    --progress     Report reading and conversion progress on stderr
-h, --help         Show help
```

Output directories must already exist. Existing output files are never overwritten. A failed import removes only output files it created. Geometry is assembled in a private workspace before publication, and JSON is written last. Publication of multiple files is not an atomic filesystem transaction.

## Georeferenced coordinates

Position decimals, including scientific notation, are parsed directly into signed 64-bit fixed-point integers with ten fractional digits. Values beyond ten fractional digits round to the nearest fixed-point unit, ties away from zero. The supported range is approximately ±922 million source units.

`--center` measures all declared positions, subtracts the center using widened integer arithmetic, then converts the local result to float32. This avoids first rounding projected world coordinates to float32. The center is rounded to the fixed-point grid; glTF geometry still has float32 precision, so very large local extents can require spatial tiling.

The origin and original bounds are recorded as decimal strings in `extras.objtools`. Restore a source position by undoing the optional axis rotation and adding the stored origin. The origin is not applied back as a scene translation, because that would defeat recentering.

The default assumes Y-up input. `--up-axis z` gives each mesh node a rotation mapping `(x, y, z)` to `(x, z, -y)` in glTF's Y-up frame. Coordinates and origin metadata remain in the source frame. Source units are assumed to be metres; no CRS, reprojection or unit conversion is inferred. For sibling assets that must stay aligned, independently centering each file will give different origins; retain and account for the recorded origins.

## Disk-backed conversion

OBJ is read once into position/UV/normal spools and a sequential face spool. Negative indices are resolved when they appear; positive references are checked against final attribute counts. Fixed-width records allow referenced attributes to be retrieved by seeking without keeping the entire model in memory.

The replay pass triangulates polygons using ear clipping and builds primitives with at most 65,536 vertices and 65,536 triangles. Each chunk has a local `(position, UV, normal)` mapping, so seams are preserved. Bounding triangle count as well as vertex count prevents repeated faces from growing memory indefinitely. Vertices can be duplicated between chunks. No whole-object deduplication or global mesh optimization is performed.

Working geometry memory is bounded by a chunk, a face and a capped 65,536-entry normal accumulator. Missing normals add one face replay pass and a 24-byte-per-source-position disk spool. JSON metadata grows with objects, materials and emitted primitives, and is currently retained in memory. The exporter supports a single object larger than RAM; it does not promise constant memory for arbitrarily many objects/primitives. OBJ lines and VRML tokens are limited to 1 MiB, VRML nesting to 1,024 levels, and polygons to 4,096 corners; oversized records are rejected explicitly.

VRML uses the existing streaming IndexedFaceSet reader to make a temporary OBJ/MTL, then follows the same pipeline. This uses extra disk space and I/O but preserves the fixed-point coordinate path. Allow workspace space for the attribute/face spools, generated binary geometry and (for VRML) intermediate OBJ/MTL. Temporary files are removed on success or error unless `--keep-tmp` is set. Seek-heavy conversion benefits from an SSD.

## Materials: Phong to PBR

Phong and PBR cannot be matched exactly. The conversion policy is intentionally explicit:

| Source | glTF mapping |
| --- | --- |
| `Kd` | Linear `baseColorFactor`; default `[0.8, 0.8, 0.8]` |
| `map_Kd` | sRGB base-color texture, multiplied by `Kd`; image policy below, URI escaped |
| `Ns` | Approximate perceptual roughness `(2 / (Ns + 2))^(1/4)` |
| `Ks` | `KHR_materials_specular`, interpreted approximately as normal-incidence reflectance |
| `Ni` | `KHR_materials_ior`; default 1.5 |
| `Pm`, `Pr` | Explicit metalness/roughness overrides |
| `Ke` | Linear emissive factor |
| `d`, `Tr` | Alpha, with `d` taking precedence regardless of property order; alpha below 1 uses `BLEND`; PNG alpha channels or `tRNS` also select `BLEND` |
| `illum 0` | `KHR_materials_unlit` |
| `illum 1` | Specular disabled |

Metalness defaults to zero: a large or colored `Ks` alone does not establish that a surface is metallic. With IOR 1.5, `Ks` is represented by `specularColorFactor = Ks / 0.04`; other IOR values use their corresponding Fresnel reflectance. IOR 1 produces zero dielectric reflectance. This preserves the approximate specular color/intensity for extension-aware viewers without inventing metalness. Core-only viewers use the metallic-roughness fallback and will render different specular responses. Extensions are listed in `extensionsUsed`, not required.

Material colors and opacity are clamped to `[0,1]`, shininess to nonnegative values, and IOR must be at least 1. Effective Phong parameters are retained under each material's `extras.sourcePhong`. Scalar colors are treated as linear; exporters that wrote sRGB scalar colors need preprocessing. Ambient `Ka` is omitted because ambient illumination belongs to the viewer's lighting. Reflection/refraction illumination modes are approximated as opaque/alpha-blended PBR rather than synthesized physical transmission.

VRML material defaults are preserved, including zero specular color, shininess 0.2, transparency and emissive color. VRML shininess is converted to an exponent using a scale of 128. `solid FALSE` becomes `doubleSided`.

Unsupported MTL maps/properties warn explicitly. `map_Kd` options, missing textures, image formats that cannot be decoded (or require disabled conversion), and textured faces without UVs are errors. Separate opacity/specular/bump maps are not baked into PBR textures. Both OBJ and VRML UVs have V flipped for glTF's texture convention.

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

OBJ objects become named meshes/nodes. Material changes, attribute-layout changes and chunk limits create separate primitives. Every primitive owns its buffer views; no vertex buffer is shared between meshes. A future glTF-side `split` can copy the referenced ranges and remap accessors, buffer views, materials and textures without reparsing source geometry. Empty objects are omitted, and OBJ `g` labels are not turned into separate meshes.

Valid source normals are preserved and normalized. Zero-length source normals are repaired with generated smooth normals and reported. Missing normals default to smooth shading: triangle normals are accumulated with area weights by source position index in a disk-backed spool, then normalized. OBJ smoothing groups (including `s off`) and VRML crease angles do not disable this default. Smoothing crosses material, UV and output-chunk boundaries for shared position indices; separate position indices are not welded. Opposing faces whose accumulated normals cancel fall back to their polygon normal. Faces mixing present and missing UVs/normals are rejected. Degenerate triangles are skipped with a count on stderr; untriangulatable polygons are errors. OBJ homogeneous positions, per-vertex colors and 3D texture coordinates are unsupported. Unsupported OBJ records warn and are omitted.

VRML coverage is IndexedFaceSet geometry with inline Coordinate, TextureCoordinate, Normal, Material and ImageTexture nodes. Index arrays may arrive in any order. Non-identity transforms, USE references, non-IndexedFaceSet primitive geometry, conditional nodes, `ccw FALSE` and `normalPerVertex FALSE` are rejected rather than silently producing misplaced geometry. Per-vertex colors warn and are omitted; texture transforms, generated UVs and animation are not implemented. Non-default fields beyond this subset are not a complete VRML scene conversion.

## Development

```sh
cargo fmt --check
RUSTFLAGS='-D warnings' cargo build --locked
cargo test --locked
cargo clippy --all-targets -- -D warnings
```

Tests cover coordinate precision, scientific notation, negative indices, UV seams, material parameters, mesh ranges, VRML defaults, concave polygons, chunk limits and output cleanup. The glTF writer uses Serde for metadata and a small binary writer, with no mesh-owning library or C/C++ dependency. CI builds and tests on Linux, macOS and Windows.

Optional format validation and a larger-than-memory stress check:

```sh
npm install --prefix /tmp/objtools-validator gltf-validator
NODE_PATH=/tmp/objtools-validator/node_modules node tools/validate_gltf.cjs terrain.gltf
cargo build --release
python3 tools/stress_import.py
```

The Unix stress script generates a single object with nine million triangles, limits the importer to 64 MiB of address space, and verifies all triangles and chunk limits. It uses roughly 1 GiB of temporary disk space.

For a real import with Linux process-memory monitoring:

```sh
python3 tools/monitor_import.py input.obj destination/model.gltf --center --progress --tmp-dir destination
NODE_PATH=/tmp/objtools-validator/node_modules node tools/validate_gltf_chunked.cjs destination/model.gltf /tmp/validation.json
```

The monitor writes import logs, one-second RSS samples and a peak-memory report beside the model. The chunked validator checks the complete JSON structure, then every primitive's binary range and referenced images using Khronos's validator, without loading the entire binary file. The metadata pass defers external-resource I/O checks to the primitive passes.
