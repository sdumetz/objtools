//! Bounded primitive assembly, independent of whole-object vertex/face counts.
use crate::{
    fixed::{format_triple, SCALE},
    import::Res,
    obj::{self, Scene},
    triangulate,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap},
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};
const MAX_VERTICES: usize = 65536;
const MAX_TRIANGLES: usize = 65536;
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct Key {
    corner: obj::Corner,
    normal: [u32; 3],
}
struct Chunk {
    object: usize,
    material: usize,
    uv: bool,
    map: HashMap<Key, u32>,
    vertices: Vec<[f32; 8]>,
    indices: Vec<u32>,
}
impl Chunk {
    fn new(object: usize, material: usize, uv: bool) -> Self {
        Self {
            object,
            material,
            uv,
            map: HashMap::new(),
            vertices: Vec::new(),
            indices: Vec::new(),
        }
    }
}
struct Document {
    accessors: Vec<Value>,
    views: Vec<Value>,
    meshes: Vec<Value>,
    offset: u64,
}
impl Document {
    fn accessor(
        &mut self,
        view: usize,
        offset: u64,
        component: u32,
        count: usize,
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
    fn flush(&mut self, chunk: &mut Chunk, out: &mut impl Write) -> Res<()> {
        if chunk.indices.is_empty() {
            return Ok(());
        }
        let stride = if chunk.uv { 32 } else { 24 };
        let start = self.offset;
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for v in &chunk.vertices {
            for i in 0..3 {
                min[i] = min[i].min(v[i]);
                max[i] = max[i].max(v[i]);
            }
            for n in &v[..stride / 4] {
                out.write_all(&n.to_le_bytes())?;
            }
        }
        let bytes = (chunk.vertices.len() * stride) as u64;
        let view = self.views.len();
        self.views.push(json!({"buffer":0,"byteOffset":start,"byteLength":bytes,"byteStride":stride,"target":34962}));
        self.offset += bytes;
        let position = self.accessor(
            view,
            0,
            5126,
            chunk.vertices.len(),
            "VEC3",
            Some((min, max)),
        );
        let normal = self.accessor(view, 12, 5126, chunk.vertices.len(), "VEC3", None);
        let mut attributes = json!({"POSITION":position,"NORMAL":normal});
        if chunk.uv {
            attributes["TEXCOORD_0"] =
                json!(self.accessor(view, 24, 5126, chunk.vertices.len(), "VEC2", None));
        }
        for i in &chunk.indices {
            out.write_all(&i.to_le_bytes())?;
        }
        let index_view = self.views.len();
        let index_bytes = (chunk.indices.len() * 4) as u64;
        self.views.push(
            json!({"buffer":0,"byteOffset":self.offset,"byteLength":index_bytes,"target":34963}),
        );
        self.offset += index_bytes;
        let indices = self.accessor(index_view, 0, 5125, chunk.indices.len(), "SCALAR", None);
        self.meshes[chunk.object]["primitives"].as_array_mut().unwrap().push(json!({"attributes":attributes,"indices":indices,"material":chunk.material,"mode":4}));
        // Every range is owned by exactly one primitive/mesh; future splitting can copy it.
        chunk.map.clear();
        chunk.vertices.clear();
        chunk.indices.clear();
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
    let origin = if center {
        scene.bbox.center().ok_or("No vertices to center")?
    } else {
        [0; 3]
    };
    eprintln!("Origin: {}", format_triple(origin));
    eprintln!(
        "Bounds: {} (size {})",
        scene.bbox.range_str().unwrap_or_default(),
        scene.bbox.size_str().unwrap_or_default()
    );
    let mut attrs = [
        File::open(dir.join("v"))?,
        File::open(dir.join("vt"))?,
        File::open(dir.join("vn"))?,
    ];
    let mut smooth = if scene.missing_normals {
        Some(crate::smooth::generate(&scene, dir, progress)?)
    } else {
        None
    };
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
    };
    // Build payload in the private workspace before touching output paths.
    let bin_spool = dir.join("payload.bin");
    let mut binary = BufWriter::with_capacity(256 * 1024, File::create(&bin_spool)?);
    let mut chunk = Chunk::new(0, 0, false);
    let mut face_count = 0u64;
    let mut skipped_degenerate = 0u64;
    let mut repaired_normals = 0u64;
    while let Some((object, material, corners)) = obj::read_face(&mut faces)? {
        face_count += 1;
        let uv = corners[0].vt != 0;
        if scene.materials[material].texture.is_some() && !uv {
            return Err(
                format!("Face {face_count}: textured material requires UV coordinates").into(),
            );
        }
        let normal = corners[0].vn != 0;
        if corners
            .iter()
            .any(|c| (c.vt != 0) != uv || (c.vn != 0) != normal)
        {
            return Err(format!("Face {face_count} mixes missing and present UVs/normals").into());
        }
        if chunk.object != object || chunk.material != material || chunk.uv != uv {
            document.flush(&mut chunk, &mut binary)?;
            chunk = Chunk::new(object, material, uv);
        }
        let mut positions = Vec::with_capacity(corners.len());
        let mut vertices = Vec::with_capacity(corners.len());
        for c in &corners {
            let raw = obj::fetch::<3>(&mut attrs[0], c.v, scene.counts[0])?.map(i64::from_le_bytes);
            let p = std::array::from_fn(|i| {
                ((raw[i] as i128 - origin[i] as i128) as f64) / (SCALE as f64)
            });
            positions.push(p);
            let mut v = [0f32; 8];
            for i in 0..3 {
                v[i] = p[i] as f32;
            }
            if normal {
                let n =
                    obj::fetch::<3>(&mut attrs[2], c.vn, scene.counts[2])?.map(f64::from_le_bytes);
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
                let t =
                    obj::fetch::<2>(&mut attrs[1], c.vt, scene.counts[1])?.map(f64::from_le_bytes);
                v[6] = t[0] as f32;
                v[7] = (1. - t[1]) as f32;
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
        if triangulate::is_degenerate_triangle(&positions) {
            skipped_degenerate += 1;
            continue;
        }
        let (triangles, flat) =
            triangulate::triangulate(&positions).map_err(|e| format!("Face {face_count}: {e}"))?;
        {
            for (corner, v) in corners.iter().zip(&mut vertices) {
                if v[3..6].iter().any(|n| *n != 0.) {
                    continue;
                }
                let sum = obj::fetch::<3>(smooth.as_mut().unwrap(), corner.v, scene.counts[0])?
                    .map(f64::from_le_bytes);
                let length = sum.iter().map(|x| x * x).sum::<f64>().sqrt();
                let normal = if length > 0. && length.is_finite() {
                    sum.map(|x| (x / length) as f32)
                } else {
                    flat
                };
                v[3..6].copy_from_slice(&normal);
            }
        }
        for triangle in triangles {
            if chunk.vertices.len() + 3 > MAX_VERTICES || chunk.indices.len() / 3 >= MAX_TRIANGLES {
                document.flush(&mut chunk, &mut binary)?;
            }
            for i in triangle {
                let v = vertices[i];
                let key = Key {
                    corner: corners[i],
                    normal: if normal {
                        [0; 3]
                    } else {
                        [v[3].to_bits(), v[4].to_bits(), v[5].to_bits()]
                    },
                };
                let index = if let Some(index) = chunk.map.get(&key) {
                    *index
                } else {
                    let index = chunk.vertices.len() as u32;
                    chunk.vertices.push(v);
                    chunk.map.insert(key, index);
                    index
                };
                chunk.indices.push(index);
            }
        }
        if progress && face_count.is_multiple_of(1_000_000) {
            eprintln!(
                "Converted {face_count} faces, {} binary bytes",
                document.offset
            );
        }
    }
    document.flush(&mut chunk, &mut binary)?;
    if repaired_normals > 0 {
        eprintln!("Warning: generated smooth normals for {repaired_normals} corners with zero-length source normals");
    }
    if skipped_degenerate > 0 {
        eprintln!("Warning: skipped {skipped_degenerate} degenerate triangles");
    }
    binary.flush()?;
    drop(binary);
    if document.offset == 0 {
        return Err("Input contains no supported triangle geometry".into());
    }
    if !center {
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
    let mut material_ids = HashMap::new();
    for (old_id, m) in scene.materials.iter().enumerate() {
        if !used_materials.contains(&old_id) {
            continue;
        }
        material_ids.insert(old_id, materials.len());
        let mut has_alpha = false;
        let texture = if let Some(path) = &m.texture {
            let path =
                fs::canonicalize(path).map_err(|e| format!("Texture {}: {e}", path.display()))?;
            let plan = &image_plans[&path];
            has_alpha = plan.alpha;
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
        if has_alpha {
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
    eprintln!(
        "Wrote {} ({face_count} faces, {} meshes, {} bytes of geometry)",
        output.display(),
        root["meshes"].as_array().unwrap().len(),
        document.offset
    );
    Ok(())
}
