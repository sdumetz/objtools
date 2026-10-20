# objtools

A Rust command-line tool for converting very large OBJ and VRML meshes to glTF 2.0 and inspecting model statistics. It handles georeferenced coordinates and models larger than available RAM, using a disk workspace.

## Install

Install from a checkout with [Rust and Cargo](https://www.rust-lang.org/tools/install):

```sh
cargo install --path . --locked
```

## Convert a model

```sh
objtools import model.obj
objtools import terrain.obj --center -o terrain.gltf
objtools import terrain.wrl --center --up-axis z -o terrain.gltf
```

An import writes a `.gltf` file, a sibling `.bin` geometry buffer and any copied or converted textures. By default, the output uses the input's name with a `.gltf` extension. The destination directory must already exist; existing output files are never overwritten.

Common options:

| Option | Purpose |
| --- | --- |
| `-o, --output FILE` | Choose the destination `.gltf` file |
| `--center` | Recenter the model; recommended for georeferenced coordinates |
| `--up-axis z` | Convert Z-up input to glTF's Y-up convention; default input is Y-up |
| `--tmp-dir DIR` | Choose a workspace on a disk with enough free space |
| `--images copy` | Copy textures alongside the output for sharing |
| `--convert-images true` | Convert all textures; defaults to JPEG at quality 95 |
| `--image-format png` | Use lossless PNG when converting, preserving transparency |
| `--progress` | Show reading and conversion progress |

Textures stay in place when input and output share a folder and no conversion is needed; otherwise they are copied. JPG and PNG are kept without conversion by default; other supported formats are converted. Source images are never modified.

Use `--convert-images true --image-format png` to preserve alpha when converting textures. JPEG conversion composites transparent pixels onto white.

## Inspect a model

```sh
objtools inspect model.obj
objtools inspect terrain.gltf --progress
```

```text
Vertices:           2.25M
Faces:              4.37M
Diffuse texels:     268M in 1 texture
Texture occupancy:  0.018

Texture                                        Size  Occupancy
Arc_de_Triomphe_FULL_u1_v1_diffuse.png  16384x16384  0.018
```

`inspect` reports the converted vertex and triangle counts, base-color texture sizes and the fraction of texture texels used by the model. OBJ and VRML inputs are converted temporarily before measuring. It accepts `.gltf` files with external buffers and images; `.glb`, embedded data and sparse accessors are unsupported.

## Working with large or georeferenced models

- Keep the workspace on a real disk with ample free space. It defaults to the output directory for `import` and the input directory for `inspect`; use `--tmp-dir` to choose another location. An SSD helps with large imports. Temporary files are cleaned up automatically.
- Use `--center` to preserve local detail in georeferenced coordinates. The original origin is stored in the glTF metadata. Files centered independently have different origins, which must be accounted for to keep them aligned.
- Coordinates are assumed to be in metres. No coordinate-system reprojection or unit conversion is performed.
- Material conversion approximates the source appearance. Unsupported data is generally omitted with a warning; VRML support covers a subset of IndexedFaceSet scenes.

## Documentation

Run `objtools import --help` or `objtools inspect --help` for all options.

The [GitHub wiki](https://github.com/sdumetz/objtools/wiki) contains the detailed documentation:

- [Command reference](https://github.com/sdumetz/objtools/wiki/Command-reference) and [image options](https://github.com/sdumetz/objtools/wiki/Images)
- [Inspection statistics and texture occupancy](https://github.com/sdumetz/objtools/wiki/Inspect)
- [Georeferenced coordinates](https://github.com/sdumetz/objtools/wiki/Georeferenced-coordinates)
- [Materials](https://github.com/sdumetz/objtools/wiki/Materials) and [geometry and format support](https://github.com/sdumetz/objtools/wiki/Geometry-and-format-support)
- [Conversion internals](https://github.com/sdumetz/objtools/wiki/Disk-backed-conversion) and [development](https://github.com/sdumetz/objtools/wiki/Development)
