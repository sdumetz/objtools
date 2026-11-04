# objtools

A Rust command-line tool for converting very large OBJ, VRML and glTF meshes to glTF 2.0 and inspecting model statistics. It handles georeferenced coordinates and models [larger than available RAM](https://github.com/sdumetz/objtools/wiki/Resource-usage-comparison), using a disk workspace.

## Install
Download a binary from latest [release](https://github.com/sdumetz/objtools/releases) or install from a repository checkout with [Rust and Cargo](https://www.rust-lang.org/tools/install):

```sh
cargo install --path . --locked
```

## Convert a model

```sh
objtools import model.obj
objtools import terrain.obj --center -o terrain.gltf
objtools import terrain.wrl --center --up-axis z -o terrain.gltf
```

An import writes a `.gltf` file, a sibling `.bin` geometry buffer and any copied or converted textures. By default, the output uses the input's name with a `.gltf` extension. The destination directory must already exist; existing output files are never overwritten. A failed import removes only output files it created.

```text
objtools import [OPTIONS] <file.obj|file.wrl|file.vrml|file.gltf|file.glb>

-o, --output FILE  Destination .gltf (default: input with .gltf extension)
    --center       Subtract the whole-input bounding-box center
    --up-axis AXIS Source up axis: y (default) or z
    --tmp-dir DIR  Workspace directory (default: the output's directory)
    --keep-tmp     Preserve the workspace for debugging
    --images MODE  auto (default), keep or copy
    --convert-images BOOL  auto (default), true or false
    --image-format FORMAT  jpg (default) or png
    --image-quality N      JPEG quality 1..100 (default 95)
    --no-color     Drop vertex colors
    --trusted      Let the model refer to files outside its folder
    --progress     Report reading and conversion progress on stderr
-h, --help         Show help
```

Textures stay in place when input and output share a folder and no conversion is needed; otherwise they are copied. JPG and PNG are kept without conversion by default; other supported formats are converted. Source images are never modified.

Use `--convert-images true --image-format png` to preserve alpha when converting textures. JPEG conversion composites transparent pixels onto white.

### glTF and GLB input

A glTF or GLB is rewritten rather than rebuilt: nodes, skins, animations, morph targets, materials and supported extensions are kept, and attributes keep their encoding, interleaving and sharing. GLB and `data:` content is unpacked into the `.bin` and image files.

The same checks apply as for other inputs. Non-finite values and out-of-range indices are errors, zero-area faces are skipped, strips and fans become triangle lists, points and lines are omitted, float normals are normalized, and a `mimeType` that contradicts the image's content is corrected with a warning. Rules that only make sense for OBJ don't apply: V isn't flipped, normals aren't generated, `alphaMode` isn't inferred and `COLOR_0` isn't renamed.

- `--center` subtracts the center from the positions when every mesh is at the world origin. Otherwise, including with skins, animation, instancing or quantized positions, a new parent node above the scene roots carries the translation. Re-importing adds to any origin an earlier import recorded.
- `--up-axis z` rotates in a new parent node above the scene roots.
- WebP, AVIF and KTX2 images stay under their extensions. `--convert-images true` converts WebP, and is an error for AVIF and KTX2, which this build can't decode.
- A file requiring an unsupported extension is rejected, including Draco and meshopt compression. Optional compression is dropped when the file also has uncompressed data, and other unknown extensions are dropped with a warning.
- Material variants (`KHR_materials_variants`) aren't supported. A warning says so, each primitive keeps its default material, and the materials and images only the variants use are dropped.

## Inspect a model

```text
objtools inspect [OPTIONS] <file.gltf|file.glb|file.obj|file.wrl|file.vrml>

    --tmp-dir DIR  Workspace for the import and coverage maps (default: the input's directory)
    --keep-tmp     Preserve the workspace for debugging
    --json         Print the statistics as JSON
    --trusted      Let the model refer to files outside its folder
    --progress     Report progress on stderr
-h, --help         Show help
```

```sh
objtools inspect model.obj
objtools inspect terrain.gltf --progress
```

example output:

```text
Vertices:           2.25M
Faces:              4.37M
Diffuse texels:     268M in 1 texture
Texture occupancy:  0.018

Texture                                        Size  Occupancy
Arc_de_Triomphe_FULL_u1_v1_diffuse.png  16384x16384  0.018
```

`--json` prints the same statistics as JSON on stdout, with exact counts and unrounded occupancy. Its structure is not documented yet and may change between versions. Warnings and progress stay on stderr.

OBJ and VRML inputs are first converted in a private workspace, then measured and cleaned up. The figures match those of the imported glTF; textures are referenced without conversion and import warnings still reach stderr.

| Statistic | Meaning |
| --- | --- |
| Vertices | glTF vertices, including the separate vertices required at UV/normal seams and material boundaries. Each mesh and shared position accessor counts once. |
| Faces | Triangles after triangulation, excluding skipped degenerate faces. Triangle strips and fans count `n − 2`; points and lines count none. |
| Diffuse texels | Width × height of each image used as a base-color texture, counting each image once. |
| Texture occupancy | Fraction of base-level texture texels sampled by the model, counting overlaps once and accounting for texture filtering, transforms and wrapping. Reported globally and per image. |

Large figures use SI multipliers (k, M, G, T) with three significant digits. Occupancy excludes lower mipmap levels. Triangles repeating a texture more than 64 times vertically are omitted from occupancy with a warning. The wiki explains the [coverage calculation](https://github.com/sdumetz/objtools/wiki/Texture-occupancy).

Supported glTF inputs are `.gltf` and `.glb`, with external, embedded or base64 `data:` buffers and images, indexed or non-indexed triangles, strips or fans, float or normalized integer UVs, sparse accessors, and the texture's selected `TEXCOORD_n`. `data:` URIs are decoded into the workspace, but their text is held in memory while the JSON is parsed, so very large models are better stored in external or GLB buffers.

Textures using `KHR_texture_basisu`, `EXT_texture_webp` or `EXT_texture_avif` are measured from the extension's image rather than its fallback, and only their headers are read. A file that requires an extension `inspect` doesn't support is rejected, which includes Draco and meshopt compression. Optional compression is skipped when the file also has uncompressed data, and any other unknown optional extension is ignored with a warning. Material variants (`KHR_materials_variants`) aren't supported, and a warning says so: occupancy comes only from each primitive's default material, so a texture only the variants use counts as unoccupied.

## Models from other people

By default, `import` and `inspect` only follow a file reference in a model (a glTF buffer or image, an OBJ `mtllib`, an MTL `map_Kd`, a VRML `url`) to a regular file inside the input's folder. A model can't read anything else on the machine, and a reference that fails gets the same message whether the file is missing or forbidden. A texture that can't be reached fails the import, as a missing one does. The output doesn't refer outside its own folder either, so `--images keep` is a command-line error (exit 2) if it would.

A reference may use `..` as long as it stays inside the folder. URLs and symlinks leading out are refused. An absolute path, which old exporters often write where a relative one belongs, is only used to look for a file of that name beside the file containing the reference. `--trusted` turns this off for a model you know: references may then point anywhere, including through symlinks, the lookup by name is only used when the absolute path doesn't exist, and `--images keep` may refer to any folder.

Messages on stderr can be shown to whoever supplied the model. A file is shown by its name, never its folder, and a reference by what the model wrote. Names from a file are cut to a short length, and characters that could drive a terminal, read as markup or hide text are replaced by `?`. A problem a file repeats gives one warning, with its count and up to three examples, once the file has been read. The texture names in the text report of `inspect` are cleaned the same way, whereas `--json` keeps them as they are, escaped as JSON.

## Exit codes

| Code | Meaning |
| ---- | ------- |
| 0 | Success |
| 1 | The input was rejected |
| 2 | The command line is wrong, including `--images keep` with an output folder that doesn't hold the input's images |
| 3 | A resource limit was reached: the disk is full, or a quota or file-size limit |
| 4 | Another problem with the machine, such as permissions, a missing output directory or an output that already exists |
| 101 | Internal error (a panic) |

Codes 3 and 4 mean the file itself may be fine, and a retry elsewhere can succeed. Every failure exits through the same cleanup. A file-size limit (`ulimit -f`) only gives code 3 if SIGXFSZ is ignored, for instance with `trap '' XFSZ` before starting objtools: otherwise the signal kills the process and leaves its workspace and partial output behind.

## Working with large or georeferenced models

- Keep the workspace on a real disk with ample free space. It defaults to the output directory for `import` and the input directory for `inspect`; use `--tmp-dir` to choose another location. An SSD helps with large imports. Temporary files are cleaned up automatically.
- Use `--center` to preserve local detail in georeferenced coordinates. The original origin is stored in the glTF metadata. Files centered independently have different origins, which must be accounted for to keep them aligned.
- Coordinates are assumed to be in metres. No coordinate-system reprojection or unit conversion is performed.
- Material conversion approximates the source appearance. Unsupported data is generally omitted with a warning; VRML support covers a subset of IndexedFaceSet scenes.

## Documentation

Run `objtools import --help` or `objtools inspect --help` for command help.

The [Wiki](https://github.com/sdumetz/objtools/wiki) covers:

- [Conversion](https://github.com/sdumetz/objtools/wiki/Conversion): supported formats, georeferencing, image formats and material approximations.
- [How it works](https://github.com/sdumetz/objtools/wiki/How-it-works): the streaming pipeline, heap and memory-mapped files, disk requirements and texture coverage calculations.
