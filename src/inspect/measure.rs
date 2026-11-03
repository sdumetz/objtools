//! Statistics read from a glTF or GLB. Buffers are mapped, and texel coverage lives in mapped
//! workspace files: nothing whole-model is held on the heap.
use super::{
    coverage::{Coverage, Filter, Wrap},
    report::{Report, TextureReport},
};
use crate::shared::{
    failure::Context,
    format::fmt_count,
    gltf::{container_size, element, extensions, index, Accessor, Gltf, Image},
    references::References,
    Res,
};
use serde_json::Value;
use std::{
    collections::{btree_map::Entry, BTreeMap, HashSet},
    fs::{self, File},
    io::{BufReader, Cursor},
    path::{Component, Path, PathBuf},
};
struct Texture {
    name: String,
    width: u32,
    height: u32,
    coverage: Coverage,
}
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
/// `path` as seen from `base`, both canonical; unchanged across filesystem roots.
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
fn dimensions(gltf: &Gltf, id: usize, display_base: &Path) -> Res<(String, u32, u32)> {
    let bytes = match gltf.image(id)? {
        Image::Bytes(bytes) => bytes,
        Image::File(path) => {
            let (width, height) = (|| -> Res<_> {
                if let Some(size) = container_size(BufReader::new(File::open(&path)?)) {
                    return Ok(size);
                }
                Ok(image::ImageReader::open(&path)?
                    .with_guessed_format()?
                    .into_dimensions()?)
            })()
            .with_context(|| format!("Texture {}", path.display()))?;
            let name = relative(&fs::canonicalize(&path)?, display_base);
            return Ok((name.display().to_string(), width, height));
        }
    };
    let (width, height) = match container_size(Cursor::new(bytes)) {
        Some(size) => size,
        None => image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()?
            .into_dimensions()?,
    };
    let name = element(&gltf.root, "images", id)?["name"]
        .as_str()
        .map_or(format!("image {id}"), str::to_owned);
    Ok((name, width, height))
}
/// Each mesh is counted once, however many nodes instance it, and a POSITION accessor shared
/// by several primitives counts its vertices once.
pub fn measure(
    path: &Path,
    references: References,
    display_base: &Path,
    workspace: &Path,
    progress: bool,
) -> Res<Report> {
    let gltf = Gltf::open(path, workspace, references)?;
    let root = &gltf.root;
    // Every image a material samples as its base colour, in image order.
    let mut textures = BTreeMap::new();
    for material in root["materials"].as_array().into_iter().flatten() {
        let info = &material["pbrMetallicRoughness"]["baseColorTexture"];
        if info.is_null() {
            continue;
        }
        let texture = element(root, "textures", index(&info["index"], "texture")?)?;
        let image = extensions::texture_source(texture)?;
        if let Entry::Vacant(slot) = textures.entry(image) {
            let (name, width, height) = dimensions(&gltf, image, display_base)?;
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
        extensions::check_primitive(root, primitive)?;
        let attributes = &primitive["attributes"];
        let position = index(&attributes["POSITION"], "POSITION accessor")?;
        let count = index(&element(root, "accessors", position)?["count"], "count")?;
        if positions.insert(position) {
            vertices += count as u64;
        }
        let indices = match primitive.get("indices") {
            Some(id) => Some(Accessor::new(&gltf, index(id, "indices")?)?),
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
        let material = element(root, "materials", index(material, "material")?)?;
        let info = &material["pbrMetallicRoughness"]["baseColorTexture"];
        if info.is_null() {
            continue;
        }
        let transform = UvTransform::new(&info["extensions"]["KHR_texture_transform"])?;
        let texture = element(root, "textures", index(&info["index"], "texture")?)?;
        // A missing sampler means repeat wrapping and engine-default (linear) filtering.
        let (wrap, filter) = match texture["sampler"].as_u64() {
            Some(id) => {
                let sampler = element(root, "samplers", usize::try_from(id)?)?;
                let mode = |key: &str| Wrap::from_gl(sampler[key].as_u64().unwrap_or(10497));
                let filter =
                    Filter::from_gl(sampler["magFilter"].as_u64(), sampler["minFilter"].as_u64());
                ([mode("wrapS")?, mode("wrapT")?], filter)
            }
            None => ([Wrap::Repeat; 2], Filter::Linear),
        };
        let image = extensions::texture_source(texture)?;
        let target = &mut textures.get_mut(&image).unwrap().coverage;
        let set = transform
            .tex_coord
            .unwrap_or(info["texCoord"].as_u64().unwrap_or(0));
        let uv = attributes
            .get(format!("TEXCOORD_{set}"))
            .ok_or_else(|| format!("Textured primitive has no TEXCOORD_{set}"))?;
        let uv = Accessor::new(&gltf, index(uv, "TEXCOORD")?)?;
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
    let textures = textures
        .into_values()
        .map(|t| TextureReport::new(t.name, t.width, t.height, t.coverage.count()))
        .collect();
    Ok(Report::new(vertices, faces, textures))
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
