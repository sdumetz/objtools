//! Bounded primitive assembly, independent of whole-object vertex/face counts.
use crate::{
    dedup::Dedup,
    fixed::{format_triple, SCALE},
    import::Res,
    obj::{self, Scene},
    triangulate,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap},
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};
/// glTF reserves each index type's maximum value for primitive restart: a primitive with
/// `UNSIGNED_SHORT` indices holds at most 65,535 vertices, one with `UNSIGNED_INT` 2^32 - 1.
const SHORT_VERTICES: u64 = u16::MAX as u64;
const MAX_VERTICES: u64 = u32::MAX as u64;
/// Floats per vertex up to the end of the UV, and where the colour starts (11 when absent).
fn layout(uv: bool, color: bool) -> (usize, usize, u64) {
    let uv_end = if uv { 8 } else { 6 };
    let color_start = if color { 8 } else { 11 };
    (
        uv_end,
        color_start,
        ((uv_end + 11 - color_start) * 4) as u64,
    )
}
/// The open primitive. Its vertices stream into the payload as they are first referenced, its
/// triangles into the index spool as u32, narrowed to u16 on close when the vertex count allows.
struct Primitive {
    object: usize,
    material: usize,
    uv: bool,
    color: bool,
    start: u64,
    vertices: u64,
    triangles: u64,
    min: [f32; 3],
    max: [f32; 3],
    indices: BufWriter<File>,
}
struct Document {
    accessors: Vec<Value>,
    views: Vec<Value>,
    meshes: Vec<Value>,
    offset: u64,
    index_spool: PathBuf,
}
impl Document {
    fn accessor(
        &mut self,
        view: usize,
        offset: u64,
        component: u32,
        count: u64,
        kind: &str,
        bounds: Option<([f32; 3], [f32; 3])>,
    ) -> usize {
        let mut a = json!({"bufferView":view,"byteOffset":offset,"componentType":component,"count":count,"type":kind});
        if let Some((min, max)) = bounds {
            a["min"] = json!(min);
            a["max"] = json!(max);
        }
        let id = self.accessors.len();
        self.accessors.push(a);
        id
    }
    fn open(
        &mut self,
        dedup: &mut Dedup,
        (object, material, uv): (usize, usize, bool),
        color: bool,
    ) -> Res<Primitive> {
        dedup.reset()?;
        Ok(Primitive {
            object,
            material,
            uv,
            color,
            start: self.offset,
            vertices: 0,
            triangles: 0,
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
            indices: BufWriter::with_capacity(256 * 1024, File::create(&self.index_spool)?),
        })
    }
    fn vertex(&mut self, p: &mut Primitive, v: &[f32; 11], out: &mut impl Write) -> Res<u32> {
        let index = u32::try_from(p.vertices)?;
        let (uv_end, color_start, stride) = layout(p.uv, p.color);
        for n in v[..uv_end].iter().chain(&v[color_start..]) {
            out.write_all(&n.to_le_bytes())?;
        }
        for ((min, max), x) in p.min.iter_mut().zip(&mut p.max).zip(v) {
            *min = min.min(*x);
            *max = max.max(*x);
        }
        p.vertices += 1;
        self.offset += stride;
        Ok(index)
    }
    fn close(&mut self, p: Primitive, out: &mut impl Write) -> Res<()> {
        if p.triangles == 0 {
            // Vertices are only written for triangles, so the payload holds nothing for it.
            return Ok(());
        }
        let (uv_end, _, stride) = layout(p.uv, p.color);
        let view = self.views.len();
        self.views.push(json!({"buffer":0,"byteOffset":p.start,"byteLength":p.vertices * stride,"byteStride":stride,"target":34962}));
        let position = self.accessor(view, 0, 5126, p.vertices, "VEC3", Some((p.min, p.max)));
        let normal = self.accessor(view, 12, 5126, p.vertices, "VEC3", None);
        let mut attributes = json!({"POSITION":position,"NORMAL":normal});
        if p.uv {
            attributes["TEXCOORD_0"] =
                json!(self.accessor(view, 24, 5126, p.vertices, "VEC2", None));
        }
        if p.color {
            let offset = uv_end as u64 * 4;
            attributes["COLOR_0"] =
                json!(self.accessor(view, offset, 5126, p.vertices, "VEC3", None));
        }
        let mut spool = p.indices.into_inner().map_err(|e| e.into_error())?;
        spool.flush()?;
        drop(spool);
        let mut spool = BufReader::with_capacity(256 * 1024, File::open(&self.index_spool)?);
        let short = p.vertices <= SHORT_VERTICES;
        let count = p.triangles * 3;
        let mut index = [0; 4];
        for _ in 0..count {
            spool.read_exact(&mut index)?;
            out.write_all(if short { &index[..2] } else { &index })?;
        }
        let bytes = count * if short { 2 } else { 4 };
        let index_view = self.views.len();
        self.views
            .push(json!({"buffer":0,"byteOffset":self.offset,"byteLength":bytes,"target":34963}));
        self.offset += bytes;
        // Keep the next vertex view 4-byte aligned after 16-bit indices.
        let padding = self.offset.next_multiple_of(4) - self.offset;
        out.write_all(&[0; 4][..padding as usize])?;
        self.offset += padding;
        let component = if short { 5123 } else { 5125 };
        let indices = self.accessor(index_view, 0, component, count, "SCALAR", None);
        self.meshes[p.object]["primitives"]
            .as_array_mut()
            .unwrap()
            .push(
                json!({"attributes":attributes,"indices":indices,"material":p.material,"mode":4}),
            );
        Ok(())
    }
}
/// URI path component escaping (not filesystem escaping).
pub fn uri(name: &str) -> String {
    let mut out = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
// Track published paths so an error removes incomplete output, without touching existing files.
struct Outputs {
    paths: Vec<PathBuf>,
    committed: bool,
}
impl Drop for Outputs {
    fn drop(&mut self) {
        if !self.committed {
            for p in &self.paths {
                let _ = fs::remove_file(p);
            }
        }
    }
}
fn create(path: &Path, outputs: &mut Outputs) -> Res<File> {
    let f = OpenOptions::new().write(true).create_new(true).open(path)?;
    outputs.paths.push(path.to_owned());
    Ok(f)
}

pub fn write(scene: Scene, dir: &Path, options: &crate::import::Options) -> Res<()> {
    write_with_limit(scene, dir, options, MAX_VERTICES)
}
/// `max_vertices` is only lowered by tests, to exercise splitting at the u32 index limit.
fn write_with_limit(
    scene: Scene,
    dir: &Path,
    options: &crate::import::Options,
    max_vertices: u64,
) -> Res<()> {
    // Validates faces before any image conversion or geometry work.
    let groups = crate::group::group(&scene, dir, options.progress)?;
    let output = &options.output;
    let center = options.center;
    let z_up = options.z_up;
    let progress = options.progress;
    let image_plans = crate::images::prepare(
        &options.images,
        &options.input,
        output,
        dir,
        &scene.materials,
    )?;
    let transparency = crate::images::scan_transparency(&image_plans);
    let origin = if center {
        scene.bbox.center().ok_or("No vertices to center")?
    } else {
        [0; 3]
    };
    let quiet = options.quiet;
    if !quiet {
        eprintln!("Origin: {}", format_triple(origin));
        eprintln!(
            "Bounds: {} (size {})",
            scene.bbox.range_str().unwrap_or_default(),
            scene.bbox.size_str().unwrap_or_default()
        );
    }
    let attrs = [
        obj::map(&dir.join("v"))?,
        obj::map(&dir.join("vt"))?,
        obj::map(&dir.join("vn"))?,
    ];
    let colors = if scene.colors {
        Some(obj::map(&dir.join("vc"))?)
    } else {
        None
    };
    let smooth = if scene.missing_normals {
        Some(crate::smooth::generate(&scene, &attrs[0], dir, progress)?)
    } else {
        None
    };
    let max_corners = groups.iter().map(|g| g.corners).max().unwrap_or(0);
    let mut dedup = Dedup::create(dir, scene.counts[0], max_corners)?;
    let mut faces = BufReader::with_capacity(256 * 1024, File::open(dir.join("faces"))?);
    let mut document = Document {
        accessors: Vec::new(),
        views: Vec::new(),
        meshes: scene
            .objects
            .iter()
            .map(|n| json!({"name":n,"primitives":[]}))
            .collect(),
        offset: 0,
        index_spool: dir.join("indices"),
    };
    // Build payload in the private workspace before touching output paths.
    let bin_spool = dir.join("payload.bin");
    let mut binary = BufWriter::with_capacity(256 * 1024, File::create(&bin_spool)?);
    let mut open: Option<Primitive> = None;
    let mut face_count = 0u64;
    let mut textured_colors = 0u64;
    let mut skipped_degenerate = 0u64;
    let mut forced_polygons = 0u64;
    let mut repaired_normals = 0u64;
    let mut splits = 0u64;
    let mut local = Vec::new();
    while let Some((object, material, corners)) = obj::read_face(&mut faces)? {
        face_count += 1;
        // Faces were validated by grouping: UV and normal presence is uniform per face.
        let uv = corners[0].vt != 0;
        let normal = corners[0].vn != 0;
        // glTF multiplies COLOR_0 into the base colour, which would tint a texture twice.
        let textured = scene.materials[material].texture.is_some();
        let color = colors.as_ref().filter(|_| !textured);
        textured_colors += u64::from(colors.is_some() && textured);
        let key = (object, material, uv);
        if open
            .as_ref()
            .is_none_or(|p| (p.object, p.material, p.uv) != key)
        {
            if let Some(p) = open.take() {
                document.close(p, &mut binary)?;
            }
            open = Some(document.open(&mut dedup, key, color.is_some())?);
        }
        let mut positions = Vec::with_capacity(corners.len());
        let mut vertices = Vec::with_capacity(corners.len());
        for c in &corners {
            let raw = obj::fetch::<3>(&attrs[0], c.v, scene.counts[0])?.map(i64::from_le_bytes);
            let p = std::array::from_fn(|i| {
                ((raw[i] as i128 - origin[i] as i128) as f64) / (SCALE as f64)
            });
            positions.push(p);
            let mut v = [0f32; 11];
            for i in 0..3 {
                v[i] = p[i] as f32;
            }
            if normal {
                let n = obj::fetch::<3>(&attrs[2], c.vn, scene.counts[2])?.map(f64::from_le_bytes);
                let length = n.iter().map(|x| x * x).sum::<f64>().sqrt();
                if !length.is_finite() {
                    return Err("Zero/invalid source normal".into());
                }
                if length > 0. {
                    for i in 0..3 {
                        v[i + 3] = (n[i] / length) as f32;
                    }
                } else {
                    repaired_normals += 1;
                }
            }
            if uv {
                let t = obj::fetch::<2>(&attrs[1], c.vt, scene.counts[1])?.map(f64::from_le_bytes);
                v[6] = t[0] as f32;
                v[7] = (1. - t[1]) as f32;
            }
            if let Some(colors) = color {
                let rgb = obj::fetch::<3>(colors, c.v, scene.counts[0])?.map(f64::from_le_bytes);
                for i in 0..3 {
                    v[i + 8] = rgb[i] as f32;
                }
            }
            if v.iter().any(|x| !x.is_finite()) {
                return Err("Attribute exceeds float32 range".into());
            }
            vertices.push(v);
        }
        // Translate relative to first corner for stable polygon calculations even without --center.
        let anchor = positions[0];
        for p in &mut positions {
            for i in 0..3 {
                p[i] -= anchor[i];
            }
        }
        let Some(polygon) = triangulate::triangulate(&positions) else {
            skipped_degenerate += 1;
            continue;
        };
        forced_polygons += u64::from(polygon.forced);
        let flat = polygon.normal;
        // A corner falling back to its face's flat normal has a normal of its own: never shared.
        let mut own = vec![false; corners.len()];
        for ((corner, v), own) in corners.iter().zip(&mut vertices).zip(&mut own) {
            if v[3..6].iter().any(|n| *n != 0.) {
                continue;
            }
            let sum = obj::fetch::<3>(smooth.as_ref().unwrap(), corner.v, scene.counts[0])?
                .map(f64::from_le_bytes);
            let length = sum.iter().map(|x| x * x).sum::<f64>().sqrt();
            let normal = if length > 0. && length.is_finite() {
                sum.map(|x| (x / length) as f32)
            } else {
                *own = true;
                flat
            };
            v[3..6].copy_from_slice(&normal);
        }
        let mut p = open.take().unwrap();
        // The only split: where u32 indices run out. A face adds at most its corner count.
        if p.vertices + corners.len() as u64 > max_vertices {
            splits += 1;
            document.close(p, &mut binary)?;
            p = document.open(&mut dedup, key, color.is_some())?;
        }
        local.clear();
        local.resize(corners.len(), None);
        for triangle in polygon.triangles {
            for i in triangle {
                let index = match local[i] {
                    Some(index) => index,
                    None => {
                        let next = u32::try_from(p.vertices)?;
                        let found = if own[i] {
                            None
                        } else {
                            dedup.find_or_insert(corners[i], next)?
                        };
                        let index = match found {
                            Some(index) => index,
                            None => document.vertex(&mut p, &vertices[i], &mut binary)?,
                        };
                        local[i] = Some(index);
                        index
                    }
                };
                p.indices.write_all(&index.to_le_bytes())?;
            }
            p.triangles += 1;
        }
        open = Some(p);
        if progress && face_count.is_multiple_of(1_000_000) {
            eprintln!(
                "Converted {face_count} faces, {} binary bytes",
                document.offset
            );
        }
    }
    if let Some(p) = open.take() {
        document.close(p, &mut binary)?;
    }
    if splits > 0 {
        eprintln!("Warning: {splits} primitives reached the {max_vertices}-vertex limit of 32-bit indices and were split");
    }
    if repaired_normals > 0 {
        eprintln!("Warning: generated smooth normals for {repaired_normals} corners with zero-length source normals");
    }
    if textured_colors > 0 {
        eprintln!("Warning: omitted vertex colors on {textured_colors} textured faces, where they would tint the texture");
    }
    if skipped_degenerate > 0 {
        eprintln!("Warning: skipped {skipped_degenerate} faces with zero area");
    }
    if forced_polygons > 0 {
        eprintln!("Warning: triangulated {forced_polygons} self-intersecting or otherwise non-simple faces by forced ear clipping; their triangles may overlap");
    }
    binary.flush()?;
    drop(binary);
    if document.offset == 0 {
        return Err("Input contains no supported triangle geometry".into());
    }
    if !center && !quiet {
        eprintln!("Note: coordinates were encoded as float32 without recentering; use --center for georeferenced assets");
    }
    let mut outputs = Outputs {
        paths: Vec::new(),
        committed: false,
    };
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut textures = Vec::new();
    let mut images = Vec::new();
    let mut texture_ids = HashMap::new();
    let mut materials = Vec::new();
    let mut extensions = BTreeSet::new();
    let used_materials: BTreeSet<usize> = document
        .meshes
        .iter()
        .flat_map(|mesh| {
            mesh["primitives"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["material"].as_u64().unwrap() as usize)
        })
        .collect();
    let transparent = transparency
        .join()
        .map_err(|_| "Texture transparency scan panicked")??;
    let mut material_ids = HashMap::new();
    for (old_id, m) in scene.materials.iter().enumerate() {
        if !used_materials.contains(&old_id) {
            continue;
        }
        material_ids.insert(old_id, materials.len());
        let mut blend = false;
        let texture = if let Some(path) = &m.texture {
            let path =
                fs::canonicalize(path).map_err(|e| format!("Texture {}: {e}", path.display()))?;
            let plan = &image_plans[&path];
            blend = transparent.contains(&path);
            if let Some(id) = texture_ids.get(&path) {
                Some(*id)
            } else {
                let id = images.len();
                if let Some(name) = &plan.publish_name {
                    let mut dst = create(&parent.join(name), &mut outputs)?;
                    std::io::copy(&mut File::open(&plan.content)?, &mut dst)?;
                }
                images.push(json!({"uri":plan.uri}));
                textures.push(json!({"source":id,"sampler":0}));
                texture_ids.insert(path, id);
                Some(id)
            }
        } else {
            None
        };
        let mut value = m.json(texture);
        if blend {
            value["alphaMode"] = json!("BLEND");
        }
        for name in value["extensions"].as_object().unwrap().keys() {
            extensions.insert(name.clone());
        }
        materials.push(value);
    }
    let mut meshes = Vec::new();
    let mut nodes = Vec::new();
    for mut mesh in document.meshes {
        if !mesh["primitives"].as_array().unwrap().is_empty() {
            for primitive in mesh["primitives"].as_array_mut().unwrap() {
                primitive["material"] =
                    json!(material_ids[&(primitive["material"].as_u64().unwrap() as usize)]);
            }
            let id = meshes.len();
            let mut node = json!({"name":mesh["name"],"mesh":id});
            if z_up {
                let q = std::f64::consts::FRAC_1_SQRT_2;
                node["rotation"] = json!([-q, 0., 0., q]);
            }
            nodes.push(node);
            meshes.push(mesh);
        }
    }
    let bin_path = output.with_extension("bin");
    let origin_strings = origin.map(crate::fixed::format_fixed_as_decimal);
    let mut root = json!({"asset":{"version":"2.0","generator":concat!("objtools ",env!("CARGO_PKG_VERSION"))},
        "scene":0,"scenes":[{"nodes":(0..nodes.len()).collect::<Vec<_>>()}],"nodes":nodes,"meshes":meshes,
        "buffers":[{"uri":uri(&bin_path.file_name().unwrap().to_string_lossy()),"byteLength":document.offset}],
        "bufferViews":document.views,"accessors":document.accessors,"materials":materials,
        "extensionsUsed":extensions,"extras":{"objtools":{"origin":origin_strings,"sourceBounds":scene.bbox.to_record(),"centered":center,"sourceUpAxis":if z_up {"z"} else {"y"},"coordinateConvention":"Source units preserved (assumed metres); no CRS inferred"}}});
    if !images.is_empty() {
        root["images"] = json!(images);
        root["textures"] = json!(textures);
        root["samplers"] = json!([{"wrapS":10497,"wrapT":10497}]);
    }
    let mut dst = BufWriter::new(create(&bin_path, &mut outputs)?);
    std::io::copy(&mut File::open(bin_spool)?, &mut dst)?;
    dst.flush()?;
    drop(dst);
    let mut dst = BufWriter::new(create(output, &mut outputs)?);
    serde_json::to_writer_pretty(&mut dst, &root)?;
    dst.flush()?;
    drop(dst);
    outputs.committed = true;
    if !quiet {
        eprintln!(
            "Wrote {} ({face_count} faces, {} meshes, {} bytes of geometry)",
            output.display(),
            root["meshes"].as_array().unwrap().len(),
            document.offset
        );
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Primitive vertex counts after importing `obj` with a lowered vertex limit.
    fn import_with_limit(obj: &str, limit: u64) -> Vec<u64> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "objtools-split-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let input = dir.join("in.obj");
        fs::write(&input, obj).unwrap();
        let output = dir.join("out.gltf");
        let options = crate::import::Options {
            input: input.clone(),
            output: output.clone(),
            tmp_dir: dir.clone(),
            center: false,
            z_up: false,
            progress: false,
            keep_tmp: false,
            images: Default::default(),
            quiet: true,
        };
        let scene = obj::spool(&input, &dir, false, None).unwrap();
        write_with_limit(scene, &dir, &options, limit).unwrap();
        let root: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        let primitives = root["meshes"][0]["primitives"].as_array().unwrap();
        let accessors = &root["accessors"];
        primitives
            .iter()
            .map(|p| {
                let position = p["attributes"]["POSITION"].as_u64().unwrap() as usize;
                accessors[position]["count"].as_u64().unwrap()
            })
            .collect()
    }
    #[test]
    fn primitives_split_only_at_the_index_limit() {
        let obj = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3\nf 1 3 4\n";
        assert_eq!(import_with_limit(obj, 6), [4]);
        // The second face could add three vertices, which would pass the limit.
        assert_eq!(import_with_limit(obj, 5), [3, 3]);
    }
}
