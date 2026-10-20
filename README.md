# objtools

A Rust command-line tool for converting very large OBJ and VRML meshes to glTF 2.0 and inspecting model statistics. It handles georeferenced coordinates and models [larger than available RAM](https://github.com/sdumetz/objtools/wiki/Resource-usage-comparison), using a disk workspace.

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

Textures stay in place when input and output share a folder and no conversion is needed; otherwise they are copied. JPG and PNG are kept without conversion by default; other supported formats are converted. Source images are never modified.

Use `--convert-images true --image-format png` to preserve alpha when converting textures. JPEG conversion composites transparent pixels onto white.

## Inspect a model

```text
objtools inspect [OPTIONS] <file.gltf|file.obj|file.wrl|file.vrml>

    --tmp-dir DIR  Workspace for the import and coverage maps (default: the input's directory)
    --keep-tmp     Preserve the workspace for debugging
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

OBJ and VRML inputs are first converted in a private workspace, then measured and cleaned up. The figures match those of the imported glTF; textures are referenced without conversion and import warnings still reach stderr.

| Statistic | Meaning |
| --- | --- |
| Vertices | glTF vertices, including the separate vertices required at UV/normal seams and material boundaries. Each mesh and shared position accessor counts once. |
| Faces | Triangles after triangulation, excluding skipped degenerate faces. Triangle strips and fans count `n − 2`; points and lines count none. |
| Diffuse texels | Width × height of each image used as a base-color texture, counting each image once. |
| Texture occupancy | Fraction of base-level texture texels sampled by the model, counting overlaps once and accounting for texture filtering, transforms and wrapping. Reported globally and per image. |

Large figures use SI multipliers (k, M, G, T) with three significant digits. Occupancy excludes lower mipmap levels. Triangles repeating a texture more than 64 times vertically are omitted from occupancy with a warning. The wiki explains the [coverage calculation](https://github.com/sdumetz/objtools/wiki/Texture-occupancy).

Supported glTF inputs use external buffers and images, indexed or non-indexed triangles, strips or fans, float or normalized integer UVs, and the texture's selected `TEXCOORD_n`. Embedded `data:` URIs, `.glb` and sparse accessors are rejected.

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
