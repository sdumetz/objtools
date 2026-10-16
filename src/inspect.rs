//! Model statistics, read from glTF. OBJ and VRML are first imported into a private workspace,
//! so every input reports exactly what `objtools import` would produce. Buffers are mapped, and
//! texel coverage lives in mapped workspace files: nothing whole-model is held on the heap.
use crate::{
    coverage::{Coverage, Filter, Wrap},
    format::fmt_count,
    import::{Res, Workspace},
};
use memmap2::Mmap;
use serde_json::Value;
use std::{
    collections::{btree_map::Entry, BTreeMap, HashSet},
    fs::{self, File},
    io::{BufReader, Cursor},
    path::{Component, Path, PathBuf},
};
pub struct Options {
    pub input: PathBuf,
    pub tmp_dir: Option<PathBuf>,
    pub keep_tmp: bool,
    pub progress: bool,
}
struct Texture {
    name: String,
    width: u32,
    height: u32,
    coverage: Coverage,
}
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
pub fn run(opts: Options) -> Res<()> {
    let ext = opts
        .input
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(ext.as_str(), "gltf" | "obj" | "wrl" | "vrml") {
        return Err(format!("Unsupported input format: .{ext}").into());
    }
    if !opts.input.is_file() {
        return Err(format!("{}: no such file", opts.input.display()).into());
    }
    // Like import's, this workspace defaults to a real disk rather than a RAM-backed /tmp.
    let tmp_dir = opts
        .tmp_dir
        .unwrap_or_else(|| parent(&opts.input).to_owned());
    let workspace = Workspace::create(&tmp_dir, opts.keep_tmp)?;
    let gltf = if ext == "gltf" {
        opts.input.clone()
    } else {
        let output = workspace.path.join("model.gltf");
        crate::import::run(crate::import::Options {
            input: opts.input.clone(),
            output: output.clone(),
            tmp_dir: workspace.path.clone(),
            center: false,
            z_up: false,
            progress: opts.progress,
            keep_tmp: opts.keep_tmp,
            images: crate::images::Options {
                mode: crate::images::Mode::Reference,
                ..Default::default()
            },
            quiet: true,
            // Colours change neither the vertices nor the texels counted.
            no_color: true,
        })?;
        output
    };
    let display_base = fs::canonicalize(parent(&opts.input))?;
    let report = inspect(&gltf, &display_base, &workspace.path, opts.progress)?;
    print!("{report}");
    Ok(())
}
/// Undo URI path escaping; `gltf::uri` writes `%XX` for every byte outside the unreserved set.
fn decode_uri(uri: &str) -> Res<PathBuf> {
    if uri.starts_with("data:") {
        return Err("Embedded data: URIs are unsupported".into());
    }
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3).ok_or("Truncated URI escape")?)?;
            out.push(u8::from_str_radix(hex, 16)?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(PathBuf::from(String::from_utf8(out)?))
}
fn index(value: &Value, what: &str) -> Res<usize> {
    let n = value
        .as_u64()
        .ok_or_else(|| format!("Missing or invalid {what} index"))?;
    Ok(usize::try_from(n)?)
}
fn element<'a>(root: &'a Value, array: &str, i: usize) -> Res<&'a Value> {
    root[array]
        .get(i)
        .ok_or_else(|| format!("{array}[{i}] does not exist").into())
}
/// A buffer view's bytes, bounds-checked against its mapped buffer.
fn view<'a>(root: &Value, buffers: &'a [Mmap], id: usize) -> Res<(&'a [u8], Option<usize>)> {
    let v = element(root, "bufferViews", id)?;
    let buffer = buffers
        .get(index(&v["buffer"], "buffer")?)
        .ok_or("Buffer view references a missing buffer")?;
    let start = usize::try_from(v["byteOffset"].as_u64().unwrap_or(0))?;
    let length = index(&v["byteLength"], "byteLength")?;
    let bytes = start
        .checked_add(length)
        .and_then(|end| buffer.get(start..end))
        .ok_or("Buffer view exceeds its buffer")?;
    let stride = v["byteStride"].as_u64().map(usize::try_from).transpose()?;
    Ok((bytes, stride))
}
struct Accessor<'a> {
    data: &'a [u8],
    count: usize,
    components: usize,
    stride: usize,
    width: usize,
    kind: u64,
    normalized: bool,
}
impl<'a> Accessor<'a> {
    fn new(root: &Value, buffers: &'a [Mmap], id: usize) -> Res<Self> {
        let a = element(root, "accessors", id)?;
        if a.get("sparse").is_some() {
            return Err(format!("Accessor {id}: sparse accessors are unsupported").into());
        }
        let count = index(&a["count"], "count")?;
        let components = match a["type"].as_str() {
            Some("SCALAR") => 1,
            Some("VEC2") => 2,
            Some("VEC3") => 3,
            Some("VEC4") => 4,
            _ => return Err(format!("Accessor {id}: unsupported type").into()),
        };
        let kind = a["componentType"].as_u64().unwrap_or(0);
        let width = match kind {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            5125 | 5126 => 4,
            _ => return Err(format!("Accessor {id}: unsupported component type").into()),
        };
        let (bytes, stride) = view(root, buffers, index(&a["bufferView"], "bufferView")?)?;
        let size = components * width;
        let stride = stride.unwrap_or(size);
        let start = usize::try_from(a["byteOffset"].as_u64().unwrap_or(0))?;
        let needed = match count {
            0 => 0,
            n => (n - 1)
                .checked_mul(stride)
                .and_then(|n| n.checked_add(size))
                .ok_or("Accessor size overflow")?,
        };
        let data = start
            .checked_add(needed)
            .and_then(|end| bytes.get(start..end))
            .ok_or_else(|| format!("Accessor {id} exceeds its buffer view"))?;
        Ok(Self {
            data,
            count,
            components,
            stride,
            width,
            kind,
            normalized: a["normalized"].as_bool().unwrap_or(false),
        })
    }
    fn get(&self, i: usize, c: usize) -> f64 {
        let o = i * self.stride + c * self.width;
        let b = &self.data[o..o + self.width];
        let (raw, max) = match self.kind {
            5120 => (f64::from(b[0] as i8), 127.),
            5121 => (f64::from(b[0]), 255.),
            5122 => (f64::from(i16::from_le_bytes([b[0], b[1]])), 32767.),
            5123 => (f64::from(u16::from_le_bytes([b[0], b[1]])), 65535.),
            5125 => (f64::from(u32::from_le_bytes(b.try_into().unwrap())), 1.),
            _ => return f64::from(f32::from_le_bytes(b.try_into().unwrap())),
        };
        if self.normalized {
            (raw / max).max(-1.)
        } else {
            raw
        }
    }
}
/// `path` as seen from `base`, both canonical; unchanged across filesystem roots.
/// `KHR_texture_transform`, the UVs a renderer actually samples with: `T · R · S · uv`, from the
/// extension's reference shader (GLSL matrices are column-major, so R's first column is
/// `(cos, sin)`).
struct UvTransform {
    rows: [[f64; 3]; 2],
    /// Overrides the texture info's `texCoord` set.
    tex_coord: Option<u64>,
}
impl UvTransform {
    fn new(ext: &Value) -> Res<Self> {
        let pair = |key: &str, default: f64| -> Res<[f64; 2]> {
            match ext.get(key) {
                None => Ok([default; 2]),
                Some(v) => {
                    let v = v.as_array().filter(|a| a.len() == 2);
                    let n = |i: usize| v.and_then(|a| a[i].as_f64());
                    match (n(0), n(1)) {
                        (Some(a), Some(b)) => Ok([a, b]),
                        _ => Err(format!("KHR_texture_transform {key} must be two numbers").into()),
                    }
                }
            }
        };
        let [ox, oy] = pair("offset", 0.)?;
        let [sx, sy] = pair("scale", 1.)?;
        let r = match ext.get("rotation") {
            None => 0.,
            Some(r) => r
                .as_f64()
                .ok_or("KHR_texture_transform rotation must be a number")?,
        };
        let (s, c) = r.sin_cos();
        Ok(Self {
            rows: [[c * sx, -s * sy, ox], [s * sx, c * sy, oy]],
            tex_coord: ext.get("texCoord").and_then(Value::as_u64),
        })
    }
    fn apply(&self, [u, v]: [f64; 2]) -> [f64; 2] {
        self.rows.map(|r| r[0] * u + r[1] * v + r[2])
    }
}
fn relative(path: &Path, base: &Path) -> PathBuf {
    let (to, from): (Vec<_>, Vec<_>) = (path.components().collect(), base.components().collect());
    let common = to.iter().zip(&from).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return path.to_owned();
    }
    let up = std::iter::repeat_n(Component::ParentDir, from.len() - common);
    up.chain(to[common..].iter().copied()).collect()
}
/// Width and height from the image header, without decoding pixels.
fn dimensions(
    root: &Value,
    base: &Path,
    buffers: &[Mmap],
    id: usize,
    display_base: &Path,
) -> Res<(String, u32, u32)> {
    let image = element(root, "images", id)?;
    if let Some(uri) = image["uri"].as_str() {
        let path = base.join(decode_uri(uri)?);
        let (width, height) = (|| -> Res<_> {
            Ok(image::ImageReader::open(&path)?
                .with_guessed_format()?
                .into_dimensions()?)
        })()
        .map_err(|e| format!("Texture {}: {e}", path.display()))?;
        let name = relative(&fs::canonicalize(&path)?, display_base);
        return Ok((name.display().to_string(), width, height));
    }
    let (bytes, _) = view(root, buffers, index(&image["bufferView"], "bufferView")?)?;
    let (width, height) = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()?
        .into_dimensions()?;
    let name = image["name"]
        .as_str()
        .map_or(format!("image {id}"), str::to_owned);
    Ok((name, width, height))
}
/// Each mesh is counted once, however many nodes instance it, and a POSITION accessor shared
/// by several primitives counts its vertices once.
fn inspect(path: &Path, display_base: &Path, workspace: &Path, progress: bool) -> Res<String> {
    let root: Value = serde_json::from_reader(BufReader::new(File::open(path)?))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let base = parent(path);
    let mut buffers = Vec::new();
    for buffer in root["buffers"].as_array().into_iter().flatten() {
        let uri = buffer["uri"]
            .as_str()
            .ok_or("Buffers without a URI (.glb) are unsupported")?;
        let path = base.join(decode_uri(uri)?);
        let map = crate::obj::map(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if (map.len() as u64) < buffer["byteLength"].as_u64().unwrap_or(0) {
            return Err(format!("{} is shorter than its byteLength", path.display()).into());
        }
        buffers.push(map);
    }
    // Every image a material samples as its base colour, in image order.
    let mut textures = BTreeMap::new();
    for material in root["materials"].as_array().into_iter().flatten() {
        let info = &material["pbrMetallicRoughness"]["baseColorTexture"];
        if info.is_null() {
            continue;
        }
        let texture = element(&root, "textures", index(&info["index"], "texture")?)?;
        let image = index(&texture["source"], "texture source image")?;
        if let Entry::Vacant(slot) = textures.entry(image) {
            let (name, width, height) = dimensions(&root, base, &buffers, image, display_base)?;
            let file = workspace.join(format!("coverage-{image}"));
            let coverage = Coverage::create(&file, width, height)?;
            slot.insert(Texture {
                name,
                width,
                height,
                coverage,
            });
        }
    }
    let mut positions = HashSet::new();
    let mut vertices = 0u64;
    let mut faces = 0u64;
    let mut rasterized = 0u64;
    let mut skipped = 0u64;
    let meshes = root["meshes"].as_array().into_iter().flatten();
    for primitive in meshes.flat_map(|m| m["primitives"].as_array().into_iter().flatten()) {
        let attributes = &primitive["attributes"];
        let position = index(&attributes["POSITION"], "POSITION accessor")?;
        let count = index(&element(&root, "accessors", position)?["count"], "count")?;
        if positions.insert(position) {
            vertices += count as u64;
        }
        let indices = match primitive.get("indices") {
            Some(id) => Some(Accessor::new(&root, &buffers, index(id, "indices")?)?),
            None => None,
        };
        if let Some(a) = &indices {
            if a.kind == 5126 || a.components != 1 || a.normalized {
                return Err("Indices must be unsigned integer scalars".into());
            }
        }
        let corners = indices.as_ref().map_or(count, |a| a.count);
        let mode = primitive["mode"].as_u64().unwrap_or(4);
        let triangles = match mode {
            4 => corners / 3,
            5 | 6 => corners.saturating_sub(2),
            _ => 0,
        };
        faces += triangles as u64;
        let Some(material) = primitive.get("material") else {
            continue;
        };
        let material = element(&root, "materials", index(material, "material")?)?;
        let info = &material["pbrMetallicRoughness"]["baseColorTexture"];
        if info.is_null() {
            continue;
        }
        let transform = UvTransform::new(&info["extensions"]["KHR_texture_transform"])?;
        let texture = element(&root, "textures", index(&info["index"], "texture")?)?;
        // A missing sampler means repeat wrapping and engine-default (linear) filtering.
        let (wrap, filter) = match texture["sampler"].as_u64() {
            Some(id) => {
                let sampler = element(&root, "samplers", usize::try_from(id)?)?;
                let mode = |key: &str| Wrap::from_gl(sampler[key].as_u64().unwrap_or(10497));
                let filter =
                    Filter::from_gl(sampler["magFilter"].as_u64(), sampler["minFilter"].as_u64());
                ([mode("wrapS")?, mode("wrapT")?], filter)
            }
            None => ([Wrap::Repeat; 2], Filter::Linear),
        };
        let image = index(&texture["source"], "texture source image")?;
        let target = &mut textures.get_mut(&image).unwrap().coverage;
        let set = transform
            .tex_coord
            .unwrap_or(info["texCoord"].as_u64().unwrap_or(0));
        let uv = attributes
            .get(format!("TEXCOORD_{set}"))
            .ok_or_else(|| format!("Textured primitive has no TEXCOORD_{set}"))?;
        let uv = Accessor::new(&root, &buffers, index(uv, "TEXCOORD")?)?;
        if uv.components != 2 || uv.count != count {
            return Err("TEXCOORD accessor must be VEC2 with one value per vertex".into());
        }
        let corner = |k: usize| -> Res<[f64; 2]> {
            let i = indices.as_ref().map_or(k, |a| a.get(k, 0) as usize);
            if i >= uv.count {
                return Err(format!("Index {i} outside {} vertices", uv.count).into());
            }
            Ok(transform.apply([uv.get(i, 0), uv.get(i, 1)]))
        };
        for t in 0..triangles {
            let ks = match mode {
                4 => [3 * t, 3 * t + 1, 3 * t + 2],
                5 => [t, t + 1, t + 2],
                _ => [0, t + 1, t + 2],
            };
            let tri = [corner(ks[0])?, corner(ks[1])?, corner(ks[2])?];
            skipped += u64::from(!target.triangle(tri, wrap, filter)?);
            rasterized += 1;
            if progress && rasterized.is_multiple_of(10_000_000) {
                eprintln!(
                    "Inspecting: rasterized {} UV triangles",
                    fmt_count(rasterized)
                );
            }
        }
    }
    if skipped > 0 {
        eprintln!("Warning: {skipped} UV triangles repeating their texture more than 64 times were left out of occupancy");
    }
    Ok(report(
        vertices,
        faces,
        textures.values().map(|t| {
            let total = u64::from(t.width) * u64::from(t.height);
            (
                t.name.as_str(),
                t.width,
                t.height,
                t.coverage.count(),
                total,
            )
        }),
    ))
}
fn ratio(used: u64, total: u64) -> String {
    if total == 0 {
        "-".into()
    } else {
        format!("{:.3}", used as f64 / total as f64)
    }
}
fn report<'a>(
    vertices: u64,
    faces: u64,
    textures: impl Iterator<Item = (&'a str, u32, u32, u64, u64)>,
) -> String {
    let textures: Vec<_> = textures.collect();
    let used: u64 = textures.iter().map(|t| t.3).sum();
    let total: u64 = textures.iter().map(|t| t.4).sum();
    let plural = if textures.len() == 1 { "" } else { "s" };
    let mut out = format!(
        "Vertices:           {}\nFaces:              {}\nDiffuse texels:     {} in {} texture{plural}\nTexture occupancy:  {}\n",
        fmt_count(vertices),
        fmt_count(faces),
        fmt_count(total),
        textures.len(),
        ratio(used, total),
    );
    if !textures.is_empty() {
        let width = textures
            .iter()
            .map(|t| t.0.chars().count())
            .max()
            .unwrap()
            .max(7);
        out.push_str(&format!(
            "\n{:width$}  {:>11}  Occupancy\n",
            "Texture", "Size"
        ));
        for (name, w, h, used, total) in textures {
            let size = format!("{w}x{h}");
            out.push_str(&format!(
                "{name:width$}  {size:>11}  {}\n",
                ratio(used, total)
            ));
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn texture_transform_follows_the_reference_shader() {
        let t = |ext: Value| UvTransform::new(&ext).unwrap();
        assert_eq!(t(Value::Null).apply([0.3, 0.7]), [0.3, 0.7]);
        let scaled = t(serde_json::json!({"offset": [0.5, -1], "scale": [2, 4], "texCoord": 1}));
        assert_eq!(scaled.apply([0.25, 0.5]), [1., 1.]);
        assert_eq!(scaled.tex_coord, Some(1));
        // Rotation applies after scale and before offset; its matrix's first column is (cos, sin).
        let rotated =
            t(serde_json::json!({"rotation": std::f64::consts::FRAC_PI_2, "scale": [2, 1]}));
        let [u, v] = rotated.apply([1., 0.]);
        assert!(u.abs() < 1e-12 && (v - 2.).abs() < 1e-12, "{u} {v}");
        assert!(UvTransform::new(&serde_json::json!({"scale": [1]})).is_err());
    }
}
