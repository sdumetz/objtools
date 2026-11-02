//! `objtools import` on glTF and GLB input.
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
static SEQ: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "objtools-gltf-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn run(&self, input: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_objtools"))
            .arg("import")
            .arg(self.0.join(input))
            .arg("-o")
            .arg(self.0.join("out.gltf"))
            .args(args)
            .output()
            .unwrap()
    }
    /// Imports `in.gltf`, returning the output and what was said on stderr.
    fn import(&self, args: &[&str]) -> (Value, Vec<u8>, String) {
        let out = self.run("in.gltf", args);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "{stderr}");
        let root = serde_json::from_slice(&fs::read(self.0.join("out.gltf")).unwrap()).unwrap();
        let bin = fs::read(self.0.join("out.bin")).unwrap();
        assert_contract(&root, &bin, &self.0);
        (root, bin, stderr)
    }
    fn fails(&self, args: &[&str], message: &str) {
        let out = self.run("in.gltf", args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "expected an error about {message}");
        assert!(stderr.contains(message), "{message}: {stderr}");
        assert!(!self.0.join("out.gltf").exists());
        assert!(!self.0.join("out.bin").exists());
        let left = fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name());
        assert!(!left.into_iter().any(|n| {
            let n = n.to_string_lossy().into_owned();
            n.starts_with("out_texture") || n.starts_with("objtools-")
        }));
    }
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
/// A stored element of an output accessor, or `None` for a sparse one, which isn't read here.
fn stored(root: &Value, bin: &[u8], accessor: usize, i: usize, c: usize) -> Option<f64> {
    let a = &root["accessors"][accessor];
    if a.get("sparse").is_some() {
        return None;
    }
    let view = &root["bufferViews"][a["bufferView"].as_u64()? as usize];
    let kind = a["componentType"].as_u64()?;
    let width = match kind {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        _ => 4,
    };
    let per = match a["type"].as_str()? {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        _ => return None,
    };
    let stride = view["byteStride"].as_u64().unwrap_or(per * width) as usize;
    let start = (view["byteOffset"].as_u64()? + a["byteOffset"].as_u64().unwrap_or(0)) as usize;
    let at = start + i * stride + c * width as usize;
    let b = &bin[at..at + width as usize];
    Some(match kind {
        5120 => f64::from(b[0] as i8),
        5121 => f64::from(b[0]),
        5122 => f64::from(i16::from_le_bytes([b[0], b[1]])),
        5123 => f64::from(u16::from_le_bytes([b[0], b[1]])),
        5125 => f64::from(u32::from_le_bytes(b.try_into().unwrap())),
        _ => f64::from(f32::from_le_bytes(b.try_into().unwrap())),
    })
}
/// The element as a renderer reads it, normalized if the accessor says so.
fn value(root: &Value, bin: &[u8], accessor: usize, i: usize, c: usize) -> Option<f64> {
    let raw = stored(root, bin, accessor, i, c)?;
    let a = &root["accessors"][accessor];
    if a["normalized"] != true {
        return Some(raw);
    }
    let max = match a["componentType"].as_u64()? {
        5120 => 127.,
        5121 => 255.,
        5122 => 32767.,
        _ => 65535.,
    };
    Some((raw / max).max(-1.))
}
/// Visits every texture slot of a material, as the importer's own walk does, independently of it.
fn visit_slots(value: &Value, visit: &mut impl FnMut(&Value)) {
    for (key, child) in value.as_object().into_iter().flatten() {
        if key == "extras" {
            continue;
        }
        if key.ends_with("Texture") && child.get("index").is_some() {
            visit(child);
        } else {
            visit_slots(child, visit);
        }
    }
}
fn extensions_in(value: &Value, found: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "extras" {
                    continue;
                }
                if key == "extensions" {
                    found.extend(
                        child
                            .as_object()
                            .into_iter()
                            .flat_map(|e| e.keys().cloned()),
                    );
                }
                extensions_in(child, found);
            }
        }
        Value::Array(list) => list.iter().for_each(|v| extensions_in(v, found)),
        _ => {}
    }
}
/// The guarantees in `src/import/from_gltf/mod.rs` that can be read back from the output alone.
/// Those relative to the source, that attributes keep their encoding and interleaving, that nodes
/// keep their indices and that required extensions stay required, have tests of their own below,
/// as do image headers beyond the signature checked here.
fn assert_contract(root: &Value, bin: &[u8], dir: &std::path::Path) {
    // Layout: one external buffer, images as files, nothing embedded.
    let buffers = root["buffers"].as_array().expect("buffers");
    assert_eq!(buffers.len(), 1, "one buffer");
    assert_eq!(buffers[0]["byteLength"], bin.len(), "buffer length");
    assert!(!buffers[0]["uri"].as_str().unwrap().starts_with("data:"));
    let images = root["images"].as_array().map_or(&[][..], Vec::as_slice);
    for image in images {
        assert!(image.get("bufferView").is_none(), "embedded image {image}");
        let uri = image["uri"].as_str().expect("image uri");
        assert!(!uri.starts_with("data:"), "embedded image");
        assert!(dir.join(uri).is_file(), "missing image file {uri}");
    }
    let views = root["bufferViews"].as_array().expect("bufferViews");
    for view in views {
        assert_eq!(view["buffer"], 0);
        let end = view["byteOffset"].as_u64().unwrap() + view["byteLength"].as_u64().unwrap();
        assert!(end <= bin.len() as u64, "view past the buffer");
    }
    // Geometry.
    let accessors = root["accessors"].as_array().expect("accessors");
    let count_of = |id: usize| accessors[id]["count"].as_u64().unwrap() as usize;
    let materials = root["materials"].as_array().map_or(&[][..], Vec::as_slice);
    let mut used_materials = BTreeSet::new();
    for mesh in root["meshes"].as_array().expect("meshes") {
        let list = mesh["primitives"].as_array().unwrap();
        assert!(!list.is_empty(), "mesh without primitives");
        for primitive in list {
            assert_eq!(primitive["mode"], 4, "triangle list");
            let attributes = primitive["attributes"].as_object().unwrap();
            let position = attributes["POSITION"].as_u64().unwrap() as usize;
            let vertices = count_of(position);
            for (name, id) in attributes {
                let id = id.as_u64().unwrap() as usize;
                assert_eq!(count_of(id), vertices, "{name} count");
                let per = accessors[id]["type"].as_str().unwrap();
                let per = per.strip_prefix("VEC").map_or(1, |n| n.parse().unwrap());
                if accessors[id]["componentType"] == 5126 {
                    for i in 0..vertices {
                        for c in 0..per {
                            if let Some(v) = stored(root, bin, id, i, c) {
                                assert!(v.is_finite(), "{name} is not finite");
                            }
                        }
                    }
                }
            }
            // Bounds match the values as stored.
            let bounds = &accessors[position];
            for c in 0..3usize {
                let values: Vec<f64> = (0..vertices)
                    .filter_map(|i| stored(root, bin, position, i, c))
                    .collect();
                if values.len() == vertices {
                    let low = values.iter().copied().fold(f64::INFINITY, f64::min);
                    let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    assert_eq!(bounds["min"][c].as_f64().unwrap() as f32, low as f32);
                    assert_eq!(bounds["max"][c].as_f64().unwrap() as f32, high as f32);
                }
            }
            // Indices: the right width, in range, whole triangles, none of zero area.
            let indices = primitive["indices"].as_u64().expect("indexed") as usize;
            let expected = if vertices <= 65535 { 5123 } else { 5125 };
            assert_eq!(accessors[indices]["componentType"], expected, "index width");
            let corners = count_of(indices);
            assert!(corners > 0 && corners % 3 == 0, "whole triangles");
            let normal = attributes
                .get("NORMAL")
                .map(|n| n.as_u64().unwrap() as usize);
            for t in 0..corners / 3 {
                let face: [usize; 3] = std::array::from_fn(|k| {
                    stored(root, bin, indices, 3 * t + k, 0).unwrap() as usize
                });
                assert!(face.iter().all(|i| *i < vertices), "index in range");
                let p = face.map(|i| -> Option<[f64; 3]> {
                    Some([
                        value(root, bin, position, i, 0)?,
                        value(root, bin, position, i, 1)?,
                        value(root, bin, position, i, 2)?,
                    ])
                });
                if let [Some(a), Some(b), Some(c)] = p {
                    let u: [f64; 3] = std::array::from_fn(|i| b[i] - a[i]);
                    let v: [f64; 3] = std::array::from_fn(|i| c[i] - a[i]);
                    let n = [
                        u[1] * v[2] - u[2] * v[1],
                        u[2] * v[0] - u[0] * v[2],
                        u[0] * v[1] - u[1] * v[0],
                    ];
                    assert!(n.iter().any(|x| *x != 0.), "zero-area triangle {face:?}");
                }
                // A float normal a triangle uses has unit length.
                if let Some(normal) = normal.filter(|n| accessors[*n]["componentType"] == 5126) {
                    for i in face {
                        let n: Option<Vec<f64>> =
                            (0..3).map(|c| stored(root, bin, normal, i, c)).collect();
                        if let Some(n) = n {
                            let length = n.iter().map(|x| x * x).sum::<f64>().sqrt();
                            assert!((length - 1.).abs() < 1e-3, "normal of length {length}");
                        }
                    }
                }
            }
            // Materials: every UV set sampled exists.
            if let Some(id) = primitive.get("material") {
                let id = id.as_u64().unwrap() as usize;
                used_materials.insert(id);
                visit_slots(&materials[id], &mut |slot| {
                    let transform = &slot["extensions"]["KHR_texture_transform"]["texCoord"];
                    let set = transform
                        .as_u64()
                        .or(slot["texCoord"].as_u64())
                        .unwrap_or(0);
                    assert!(
                        attributes.contains_key(&format!("TEXCOORD_{set}")),
                        "UV set {set}"
                    );
                });
            }
        }
    }
    // Everything published is used.
    assert_eq!(used_materials.len(), materials.len(), "unused material");
    let mut used_textures = BTreeSet::new();
    for material in materials {
        visit_slots(material, &mut |slot| {
            used_textures.insert(slot["index"].as_u64().unwrap());
        });
    }
    let textures = root["textures"].as_array().map_or(&[][..], Vec::as_slice);
    assert_eq!(used_textures.len(), textures.len(), "unused texture");
    let mut used_images = BTreeSet::new();
    for texture in textures {
        used_images.extend(texture["source"].as_u64());
        for (_, extension) in texture["extensions"].as_object().into_iter().flatten() {
            used_images.extend(extension["source"].as_u64());
        }
    }
    assert_eq!(used_images.len(), images.len(), "unused image");
    // Images: core formats, or the format of the extension selecting them.
    let head = |id: u64| -> Vec<u8> {
        let uri = images[id as usize]["uri"].as_str().unwrap();
        fs::read(dir.join(uri)).unwrap()
    };
    for texture in textures {
        if let Some(id) = texture["source"].as_u64() {
            let bytes = head(id);
            let core = bytes.starts_with(&[0xff, 0xd8]) || bytes.starts_with(b"\x89PNG");
            assert!(core, "image {id} is neither JPG nor PNG");
        }
        for (name, extension) in texture["extensions"].as_object().into_iter().flatten() {
            let Some(id) = extension["source"].as_u64() else {
                continue;
            };
            let bytes = head(id);
            let matches = match name.as_str() {
                "EXT_texture_webp" => bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
                "EXT_texture_avif" => &bytes[4..8] == b"ftyp",
                "KHR_texture_basisu" => bytes.starts_with(b"\xabKTX 20"),
                _ => true,
            };
            assert!(matches, "image {id} isn't the format of {name}");
        }
    }
    for image in images {
        let Some(declared) = image["mimeType"].as_str() else {
            continue;
        };
        let bytes = fs::read(dir.join(image["uri"].as_str().unwrap())).unwrap();
        let real = if bytes.starts_with(&[0xff, 0xd8]) {
            "image/jpeg"
        } else if bytes.starts_with(b"\x89PNG") {
            "image/png"
        } else if bytes.starts_with(b"RIFF") {
            "image/webp"
        } else if bytes.starts_with(b"\xabKTX 20") {
            "image/ktx2"
        } else {
            "image/avif"
        };
        assert_eq!(declared, real, "wrong mimeType");
    }
    // Extensions: the lists say exactly what is there.
    let listed = |key: &str| -> BTreeSet<String> {
        let list = root[key].as_array().into_iter().flatten();
        list.map(|v| v.as_str().unwrap().to_owned()).collect()
    };
    let (used, required) = (listed("extensionsUsed"), listed("extensionsRequired"));
    let mut found = BTreeSet::new();
    extensions_in(root, &mut found);
    let mut without_quantization = used.clone();
    without_quantization.remove("KHR_mesh_quantization");
    assert_eq!(without_quantization, found, "extensionsUsed");
    assert!(required.is_subset(&used), "extensionsRequired");
    if used.contains("KHR_texture_transform") {
        assert!(
            required.contains("KHR_texture_transform"),
            "transform required"
        );
    }
    for name in &used {
        assert!(
            !name.starts_with("VENDOR") && !name.contains("compression"),
            "{name}"
        );
    }
    // Hierarchy: a strict tree, scene roots without parents.
    let nodes = root["nodes"].as_array().map_or(&[][..], Vec::as_slice);
    let mut parents = vec![0; nodes.len()];
    for node in nodes {
        for child in node["children"].as_array().into_iter().flatten() {
            parents[child.as_u64().unwrap() as usize] += 1;
        }
    }
    assert!(parents.iter().all(|p| *p <= 1), "a node with two parents");
    for scene in root["scenes"].as_array().expect("scenes") {
        for node in scene["nodes"].as_array().into_iter().flatten() {
            assert_eq!(
                parents[node.as_u64().unwrap() as usize],
                0,
                "scene root with a parent"
            );
        }
    }
    // Georeferencing.
    let record = &root["extras"]["objtools"];
    let origin = record["origin"].as_array().expect("origin");
    assert_eq!(origin.len(), 3);
    assert!(origin
        .iter()
        .all(|c| c.as_str().unwrap().parse::<f64>().is_ok()));
    assert!(record["sourceBounds"].is_object(), "sourceBounds");
    assert!(record["centered"].is_boolean());
}
/// The contract on real files, which the fixtures here can't replace:
/// `OBJTOOLS_SAMPLES=dir cargo test --test import_gltf -- --ignored`. Files needing what isn't
/// supported, such as compression, only have to fail cleanly.
#[test]
#[ignore = "needs OBJTOOLS_SAMPLES"]
fn contract_holds_on_sample_files() {
    let samples = PathBuf::from(std::env::var("OBJTOOLS_SAMPLES").expect("OBJTOOLS_SAMPLES"));
    let mut names: Vec<_> = fs::read_dir(&samples)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "gltf" || e == "glb"))
        .collect();
    names.sort();
    assert!(!names.is_empty());
    for input in names {
        for args in [&[][..], &["--center"], &["--up-axis", "z"]] {
            let f = Fixture::new();
            let out = Command::new(env!("CARGO_BIN_EXE_objtools"))
                .arg("import")
                .arg(&input)
                .arg("-o")
                .arg(f.0.join("out.gltf"))
                .args(["--images", "copy"])
                .args(args)
                .output()
                .unwrap();
            let name = input.display();
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert!(stderr.contains("Error"), "{name}: {stderr}");
                assert!(
                    !f.0.join("out.gltf").exists(),
                    "{name}: output after failure"
                );
                println!("{name} {args:?}: refused: {}", stderr.trim());
                continue;
            }
            let root: Value =
                serde_json::from_slice(&fs::read(f.0.join("out.gltf")).unwrap()).unwrap();
            assert_contract(&root, &fs::read(f.0.join("out.bin")).unwrap(), &f.0);
            println!("{name} {args:?}: contract holds");
        }
    }
}
/// A glTF under construction, with one mesh on one node.
struct Model {
    root: Value,
    bin: Vec<u8>,
}
impl Model {
    fn new() -> Self {
        Self {
            root: json!({
                "asset": {"version": "2.0", "generator": "test", "copyright": "kept"},
                "scene": 0,
                "scenes": [{"nodes": [0]}],
                "nodes": [{"mesh": 0}],
                "meshes": [{"primitives": []}],
                "accessors": [],
                "bufferViews": [],
            }),
            bin: Vec::new(),
        }
    }
    fn data(&mut self, bytes: &[u8], kind: &str, component: u64, count: usize) -> usize {
        self.bin.resize(self.bin.len().next_multiple_of(4), 0);
        let view = self.root["bufferViews"].as_array().unwrap().len();
        let (offset, length) = (self.bin.len(), bytes.len());
        self.root["bufferViews"]
            .as_array_mut()
            .unwrap()
            .push(json!({"buffer": 0, "byteOffset": offset, "byteLength": length}));
        self.bin.extend(bytes);
        let accessors = self.root["accessors"].as_array_mut().unwrap();
        accessors.push(
            json!({"bufferView": view, "componentType": component, "count": count, "type": kind}),
        );
        accessors.len() - 1
    }
    fn floats(&mut self, values: &[f32], kind: &str, per: usize) -> usize {
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.data(&bytes, kind, 5126, values.len() / per)
    }
    fn positions(&mut self, values: &[f32]) -> usize {
        let id = self.floats(values, "VEC3", 3);
        let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in values.chunks(3) {
            for c in 0..3 {
                min[c] = min[c].min(p[c]);
                max[c] = max[c].max(p[c]);
            }
        }
        self.root["accessors"][id]["min"] = json!(min);
        self.root["accessors"][id]["max"] = json!(max);
        id
    }
    fn shorts(&mut self, values: &[u16]) -> usize {
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.data(&bytes, "SCALAR", 5123, values.len())
    }
    fn primitive(&mut self, primitive: Value) {
        self.root["meshes"][0]["primitives"]
            .as_array_mut()
            .unwrap()
            .push(primitive);
    }
    /// One triangle, indexed.
    fn triangle() -> Self {
        let mut model = Self::new();
        let position = model.positions(&[0., 0., 0., 1., 0., 0., 0., 1., 0.]);
        let indices = model.shorts(&[0, 1, 2]);
        model.primitive(json!({"attributes": {"POSITION": position}, "indices": indices}));
        model
    }
    /// A triangle with UVs and normals, using `material` and an 8x8 PNG named `tex.png`.
    fn textured(material: Value) -> (Self, Fixture) {
        let mut model = Self::new();
        let position = model.positions(&[0., 0., 0., 1., 0., 0., 0., 1., 0.]);
        let uv = model.floats(&[0., 0., 1., 0., 0., 1.], "VEC2", 2);
        let normal = model.floats(&[0., 0., 1., 0., 0., 1., 0., 0., 1.], "VEC3", 3);
        let indices = model.shorts(&[0, 1, 2]);
        model.primitive(json!({
            "attributes": {"POSITION": position, "TEXCOORD_0": uv, "NORMAL": normal},
            "indices": indices,
            "material": 0,
        }));
        model.root["materials"] = json!([material]);
        model.root["textures"] = json!([{"source": 0}]);
        model.root["images"] = json!([{"uri": "tex.png"}]);
        let f = Fixture::new();
        image::RgbImage::new(8, 8)
            .save(f.0.join("tex.png"))
            .unwrap();
        (model, f)
    }
    /// `in.gltf` and `in.bin` in a new fixture.
    fn write(self) -> Fixture {
        let f = Fixture::new();
        self.write_to(&f);
        f
    }
    fn write_to(mut self, f: &Fixture) {
        self.root["buffers"] = json!([{"uri": "in.bin", "byteLength": self.bin.len()}]);
        fs::write(f.0.join("in.gltf"), self.root.to_string()).unwrap();
        fs::write(f.0.join("in.bin"), &self.bin).unwrap();
    }
}
/// The numbers of an accessor in the output, element by element.
fn floats(root: &Value, bin: &[u8], accessor: u64) -> Vec<Vec<f32>> {
    let a = &root["accessors"][accessor as usize];
    let view = &root["bufferViews"][a["bufferView"].as_u64().unwrap() as usize];
    let per = match a["type"].as_str().unwrap() {
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        _ => 1,
    };
    let stride = view["byteStride"].as_u64().unwrap_or(per * 4);
    let start = view["byteOffset"].as_u64().unwrap() + a["byteOffset"].as_u64().unwrap_or(0);
    (0..a["count"].as_u64().unwrap())
        .map(|i| {
            (0..per)
                .map(|c| {
                    let o = (start + i * stride + c * 4) as usize;
                    f32::from_le_bytes(bin[o..o + 4].try_into().unwrap())
                })
                .collect()
        })
        .collect()
}
fn indices(root: &Value, bin: &[u8], primitive: &Value) -> Vec<u32> {
    let a = &root["accessors"][primitive["indices"].as_u64().unwrap() as usize];
    let view = &root["bufferViews"][a["bufferView"].as_u64().unwrap() as usize];
    let start = view["byteOffset"].as_u64().unwrap() as usize;
    let count = a["count"].as_u64().unwrap() as usize;
    match a["componentType"].as_u64().unwrap() {
        5123 => bin[start..start + count * 2]
            .chunks(2)
            .map(|b| u16::from_le_bytes(b.try_into().unwrap()).into())
            .collect(),
        _ => bin[start..start + count * 4]
            .chunks(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect(),
    }
}
fn primitive(root: &Value) -> &Value {
    &root["meshes"][0]["primitives"][0]
}
fn inspect(f: &Fixture, input: &str) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_objtools"))
        .args(["inspect", "--json"])
        .arg(f.0.join(input))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
/// A report on `input`, whose embedded image is named differently once it is a file.
fn inspect_named(f: &Fixture, input: &str, name: &str) -> Value {
    let mut report = inspect(f, input);
    report["textures"][0]["name"] = json!(name);
    report
}
#[test]
fn output_keeps_the_model_and_records_the_import() {
    let mut model = Model::triangle();
    model.root["asset"]["extras"] = json!({"author": "kept"});
    model.root["extras"] = json!({"mine": 1});
    model.root["nodes"][0]["name"] = json!("node");
    model.root["cameras"] =
        json!([{"type": "perspective", "perspective": {"yfov": 1, "znear": 1}}]);
    let f = model.write();
    let (root, _, stderr) = f.import(&[]);
    assert_eq!(root["asset"]["copyright"], "kept");
    assert_eq!(root["asset"]["extras"]["author"], "kept");
    assert!(root["asset"]["generator"]
        .as_str()
        .unwrap()
        .starts_with("objtools"));
    assert_eq!(root["nodes"][0]["name"], "node");
    assert_eq!(root["cameras"][0]["type"], "perspective");
    assert_eq!(root["extras"]["mine"], 1);
    assert_eq!(root["extras"]["objtools"]["centered"], false);
    assert_eq!(root["extras"]["objtools"]["sourceUpAxis"], "y");
    assert_eq!(root["extras"]["objtools"]["origin"], json!(["0", "0", "0"]));
    assert_eq!(
        root["buffers"],
        json!([{"uri": "out.bin", "byteLength": fs::read(f.0.join("out.bin")).unwrap().len()}])
    );
    assert!(stderr.contains("Origin: 0 0 0"), "{stderr}");
    assert!(stderr.contains("Bounds: 0 0 0 → 1 1 0"), "{stderr}");
    assert!(stderr.contains("without recentering"), "{stderr}");
}
#[test]
fn an_existing_output_is_refused_including_the_input_itself() {
    let f = Model::triangle().write();
    let out = Command::new(env!("CARGO_BIN_EXE_objtools"))
        .arg("import")
        .arg(f.0.join("in.gltf"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("already exists"));
    let (root, _, _) = f.import(&[]);
    assert_eq!(root["meshes"][0]["primitives"][0]["mode"], 4);
    let again = f.run("in.gltf", &[]);
    assert!(!again.status.success());
}
#[test]
fn glb_and_data_uris_are_unpacked() {
    let png = {
        let f = Fixture::new();
        image::RgbImage::new(8, 8).save(f.0.join("a.png")).unwrap();
        fs::read(f.0.join("a.png")).unwrap()
    };
    let (mut model, f) =
        Model::textured(json!({"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}));
    fs::remove_file(f.0.join("tex.png")).unwrap();
    let geometry = model.bin.len();
    let view = model.root["bufferViews"].as_array().unwrap().len();
    model.bin.resize(model.bin.len().next_multiple_of(4), 0);
    model.root["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "buffer": 0, "byteOffset": model.bin.len(), "byteLength": png.len()
        }));
    model.bin.extend(&png);
    model.root["images"] = json!([{"bufferView": view, "mimeType": "image/png", "name": "paint"}]);
    model.root["buffers"] = json!([{"byteLength": model.bin.len()}]);
    let mut json_chunk = model.root.to_string().into_bytes();
    json_chunk.resize(json_chunk.len().next_multiple_of(4), b' ');
    model.bin.resize(model.bin.len().next_multiple_of(4), 0);
    let mut file = b"glTF".to_vec();
    file.extend(2u32.to_le_bytes());
    file.extend(((28 + json_chunk.len() + model.bin.len()) as u32).to_le_bytes());
    file.extend((json_chunk.len() as u32).to_le_bytes());
    file.extend(b"JSON");
    file.extend(&json_chunk);
    file.extend((model.bin.len() as u32).to_le_bytes());
    file.extend(b"BIN\0");
    file.extend(&model.bin);
    fs::write(f.0.join("in.glb"), file).unwrap();
    let out = f.run("in.glb", &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let root: Value = serde_json::from_slice(&fs::read(f.0.join("out.gltf")).unwrap()).unwrap();
    let bin = fs::read(f.0.join("out.bin")).unwrap();
    assert_contract(&root, &bin, &f.0);
    // The image is a file of its own, and out of the buffer.
    assert_eq!(root["images"][0]["uri"], "out_texture_0.png");
    assert_eq!(root["images"][0]["name"], "paint");
    assert!(root["images"][0].get("bufferView").is_none());
    assert_eq!(fs::read(f.0.join("out_texture_0.png")).unwrap(), png);
    assert!(bin.len() <= geometry + 12, "{} > {geometry}", bin.len());
    assert_eq!(
        inspect(&f, "out.gltf"),
        inspect_named(&f, "in.glb", "out_texture_0.png")
    );
    // The same model as data: URIs, keeping `--images keep` from referencing a file that is gone.
    let g = Fixture::new();
    let encode = |bytes: &[u8]| {
        const LETTERS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let group =
                chunk.iter().fold(0u32, |n, b| n << 8 | u32::from(*b)) << (8 * (3 - chunk.len()));
            for i in 0..4 {
                out.push(if i <= chunk.len() {
                    LETTERS[(group >> (18 - 6 * i) & 63) as usize] as char
                } else {
                    '='
                });
            }
        }
        out
    };
    let mut embedded = model.root.clone();
    embedded["buffers"][0]["uri"] = json!(format!(
        "data:application/octet-stream;base64,{}",
        encode(&model.bin)
    ));
    embedded["images"] = json!([{"uri": format!("data:image/png;base64,{}", encode(&png))}]);
    fs::write(g.0.join("in.gltf"), embedded.to_string()).unwrap();
    let (root, _, _) = g.import(&["--images", "keep"]);
    assert_eq!(root["images"][0]["uri"], "out_texture_0.png");
    assert!(!fs::read_to_string(g.0.join("out.gltf"))
        .unwrap()
        .contains("data:"));
    assert_eq!(fs::read(g.0.join("out_texture_0.png")).unwrap(), png);
}
#[test]
fn strips_and_fans_become_lists_with_the_spec_s_winding() {
    let mut model = Model::new();
    let position = model.positions(&[0., 0., 0., 1., 0., 0., 0., 1., 0., 1., 1., 0.]);
    let indices = model.shorts(&[0, 1, 2, 3]);
    model.primitive(json!({"attributes": {"POSITION": position}, "indices": indices, "mode": 5}));
    model.primitive(json!({"attributes": {"POSITION": position}, "mode": 6}));
    let f = model.write();
    let (root, bin, _) = f.import(&[]);
    let list = root["meshes"][0]["primitives"].as_array().unwrap();
    assert_eq!(list[0]["mode"], 4);
    assert_eq!(indices_of(&root, &bin, &list[0]), [0, 1, 2, 1, 3, 2]);
    assert_eq!(list[1]["mode"], 4);
    assert_eq!(indices_of(&root, &bin, &list[1]), [1, 2, 0, 2, 3, 0]);
    // Both still use the one position accessor.
    assert_eq!(
        list[0]["attributes"]["POSITION"],
        list[1]["attributes"]["POSITION"]
    );
}
fn indices_of(root: &Value, bin: &[u8], primitive: &Value) -> Vec<u32> {
    indices(root, bin, primitive)
}
#[test]
fn points_lines_and_empty_meshes_are_omitted_with_one_warning() {
    let mut model = Model::triangle();
    let position = model.root["meshes"][0]["primitives"][0]["attributes"]["POSITION"].clone();
    for mode in 0..4 {
        model.primitive(json!({"attributes": {"POSITION": position}, "mode": mode}));
    }
    model.primitive(json!({"attributes": {}}));
    model.root["meshes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"primitives": [{"attributes": {"POSITION": position}, "mode": 1}]}));
    model.root["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"mesh": 1, "weights": [1], "name": "lines"}));
    model.root["scenes"][0]["nodes"] = json!([0, 1]);
    let f = model.write();
    let (root, _, stderr) = f.import(&[]);
    assert_eq!(root["meshes"].as_array().unwrap().len(), 1);
    assert_eq!(root["meshes"][0]["primitives"].as_array().unwrap().len(), 1);
    // The node stays, as other things may refer to it, without what needs a mesh.
    assert_eq!(root["nodes"][1], json!({"name": "lines"}));
    assert_eq!(
        stderr
            .matches("omitted 5 primitives of points or lines")
            .count(),
        1,
        "{stderr}"
    );
    assert_eq!(
        stderr
            .matches("omitted 1 primitives without POSITION")
            .count(),
        1,
        "{stderr}"
    );
}
#[test]
fn input_without_triangles_is_an_error() {
    let mut model = Model::new();
    let position = model.positions(&[0., 0., 0.]);
    model.primitive(json!({"attributes": {"POSITION": position}, "mode": 0}));
    model.write().fails(&[], "no supported triangle geometry");
}
#[test]
fn zero_area_faces_are_skipped_and_counted_but_vertices_stay() {
    let mut model = Model::new();
    let position = model.positions(&[0., 0., 0., 1., 0., 0., 0., 1., 0., 2., 0., 0.]);
    // A repeated corner, and three collinear ones.
    let indices = model.shorts(&[0, 1, 2, 0, 0, 1, 0, 1, 3]);
    model.primitive(json!({"attributes": {"POSITION": position}, "indices": indices}));
    let f = model.write();
    let (root, bin, stderr) = f.import(&[]);
    assert_eq!(indices_of(&root, &bin, primitive(&root)), [0, 1, 2]);
    assert_eq!(
        root["accessors"][primitive(&root)["attributes"]["POSITION"].as_u64().unwrap() as usize]
            ["count"],
        4
    );
    assert!(
        stderr.contains("skipped 2 faces with zero area"),
        "{stderr}"
    );
}
#[test]
fn zero_area_faces_are_counted_for_each_primitive_sharing_them() {
    let mut model = Model::new();
    let position = model.positions(&[0., 0., 0., 1., 0., 0., 0., 1., 0.]);
    let indices = model.shorts(&[0, 1, 2, 0, 0, 1]);
    for _ in 0..3 {
        model.primitive(json!({"attributes": {"POSITION": position}, "indices": indices}));
    }
    let f = model.write();
    let (root, _, stderr) = f.import(&[]);
    assert_eq!(root["meshes"][0]["primitives"].as_array().unwrap().len(), 3);
    assert!(
        stderr.contains("skipped 3 faces with zero area"),
        "{stderr}"
    );
}
#[test]
fn index_width_follows_the_vertex_count() {
    // Bytes widen, as glTF renderers are given no 8-bit indices by the OBJ path either.
    let mut model = Model::new();
    let position = model.positions(&[0., 0., 0., 1., 0., 0., 0., 1., 0.]);
    let bytes = model.data(&[0, 1, 2, 0], "SCALAR", 5121, 3);
    model.primitive(json!({"attributes": {"POSITION": position}, "indices": bytes}));
    let f = model.write();
    let (root, bin, _) = f.import(&[]);
    let id = primitive(&root)["indices"].as_u64().unwrap() as usize;
    assert_eq!(root["accessors"][id]["componentType"], 5123);
    assert_eq!(indices_of(&root, &bin, primitive(&root)), [0, 1, 2]);
    // 32-bit indices narrow when the vertices allow it.
    let mut model = Model::new();
    let position = model.positions(&[0., 0., 0., 1., 0., 0., 0., 1., 0.]);
    let wide: Vec<u8> = [0u32, 1, 2].iter().flat_map(|v| v.to_le_bytes()).collect();
    let wide = model.data(&wide, "SCALAR", 5125, 3);
    model.primitive(json!({"attributes": {"POSITION": position}, "indices": wide}));
    let (root, _, _) = model.write().import(&[]);
    let id = primitive(&root)["indices"].as_u64().unwrap() as usize;
    assert_eq!(root["accessors"][id]["componentType"], 5123);
    // 65,536 vertices need 32 bits, since 65,535 is reserved for primitive restart.
    let mut model = Model::new();
    let mut points = vec![0f32; 65536 * 3];
    points[3] = 1.;
    points[7] = 1.;
    let position = model.positions(&points);
    let indices = model.shorts(&[0, 1, 2]);
    model.primitive(json!({"attributes": {"POSITION": position}, "indices": indices}));
    let (root, _, _) = model.write().import(&[]);
    let id = primitive(&root)["indices"].as_u64().unwrap() as usize;
    assert_eq!(root["accessors"][id]["componentType"], 5125);
}
#[test]
fn non_indexed_primitives_get_sequential_indices() {
    let mut model = Model::new();
    let position = model.positions(&[0., 0., 0., 1., 0., 0., 0., 1., 0.]);
    model.primitive(json!({"attributes": {"POSITION": position}}));
    let (root, bin, _) = model.write().import(&[]);
    assert_eq!(indices_of(&root, &bin, primitive(&root)), [0, 1, 2]);
}
#[test]
fn invalid_geometry_is_an_error_and_publishes_nothing() {
    let mut model = Model::new();
    let position = model.positions(&[0., 0., 0., 1., 0., 0., 0., 1., 0.]);
    let indices = model.shorts(&[0, 1, 3]);
    model.primitive(json!({"attributes": {"POSITION": position}, "indices": indices}));
    model.write().fails(&[], "Index 3 outside 3 vertices");

    let mut model = Model::new();
    let position = model.positions(&[0., 0., 0., 1., 0., 0., f32::NAN, 1., 0.]);
    model.primitive(json!({"attributes": {"POSITION": position}}));
    model.write().fails(&[], "non-finite");

    let mut model = Model::triangle();
    let uv = model.floats(&[0., 0., f32::INFINITY, 0., 0., 1.], "VEC2", 2);
    model.root["meshes"][0]["primitives"][0]["attributes"]["TEXCOORD_0"] = json!(uv);
    model.write().fails(&[], "non-finite");

    let mut model = Model::triangle();
    let uv = model.floats(&[0., 0.], "VEC2", 2);
    model.root["meshes"][0]["primitives"][0]["attributes"]["TEXCOORD_0"] = json!(uv);
    model.write().fails(&[], "different count");

    let mut model = Model::triangle();
    model.root["nodes"] = json!([{"mesh": 0, "children": [0]}]);
    model.write().fails(&[], "cycle");

    let mut model = Model::triangle();
    model.root["nodes"] = json!([{"children": [2]}, {"children": [2]}, {"mesh": 0}]);
    model.root["scenes"][0]["nodes"] = json!([0, 1]);
    model.write().fails(&[], "more than one parent");
}
#[test]
fn position_bounds_are_recomputed_and_other_accessors_keep_theirs() {
    let mut model = Model::triangle();
    model.root["accessors"][0]["min"] = json!([-100, -100, -100]);
    model.root["accessors"][0]["max"] = json!([100, 100, 100]);
    let f = model.write();
    let (root, _, _) = f.import(&[]);
    let id = primitive(&root)["attributes"]["POSITION"].as_u64().unwrap() as usize;
    assert_eq!(root["accessors"][id]["min"], json!([0., 0., 0.]));
    assert_eq!(root["accessors"][id]["max"], json!([1., 1., 0.]));
}
#[test]
fn attributes_keep_their_encoding_interleaving_and_sharing() {
    // A quantized position and normalized UV, interleaved with a padded stride.
    let mut model = Model::new();
    let mut bytes = Vec::new();
    for (p, uv) in [
        ([0u16, 0, 0], [0u16, 0]),
        ([1, 0, 0], [65535, 0]),
        ([0, 1, 0], [0, 65535]),
    ] {
        p.iter()
            .chain(&[0u16])
            .chain(&uv)
            .for_each(|v| bytes.extend(v.to_le_bytes()));
    }
    model.bin = bytes;
    model.root["bufferViews"] = json!([{"buffer": 0, "byteLength": model.bin.len(), "byteStride": 12, "target": 34962, "name": "kept"}]);
    model.root["accessors"] = json!([
        {"bufferView": 0, "componentType": 5123, "count": 3, "type": "VEC3", "min": [0, 0, 0], "max": [9, 9, 9]},
        {"bufferView": 0, "byteOffset": 8, "componentType": 5123, "normalized": true, "count": 3, "type": "VEC2"},
    ]);
    let indices = model.shorts(&[0, 1, 2]);
    // The same accessors twice, which must not be copied twice.
    model.primitive(json!({"attributes": {"POSITION": 0, "TEXCOORD_0": 1}, "indices": indices}));
    model.primitive(json!({"attributes": {"POSITION": 0, "TEXCOORD_0": 1}, "indices": indices}));
    model.root["extensionsUsed"] = json!(["KHR_mesh_quantization"]);
    model.root["extensionsRequired"] = json!(["KHR_mesh_quantization"]);
    let source_bytes = model.bin[..36].to_vec();
    let f = model.write();
    let (root, bin, _) = f.import(&[]);
    let list = root["meshes"][0]["primitives"].as_array().unwrap();
    assert_eq!(list[0]["attributes"], list[1]["attributes"]);
    assert_eq!(list[0]["indices"], list[1]["indices"]);
    let position = list[0]["attributes"]["POSITION"].as_u64().unwrap() as usize;
    let accessor = &root["accessors"][position];
    assert_eq!(accessor["componentType"], 5123);
    // Bounds of a quantized accessor are its stored integers.
    assert_eq!(accessor["min"], json!([0, 0, 0]));
    assert_eq!(accessor["max"], json!([1, 1, 0]));
    let view = &root["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
    assert_eq!(view["byteStride"], 12);
    assert_eq!(view["name"], "kept");
    let start = view["byteOffset"].as_u64().unwrap() as usize;
    assert_eq!(bin[start..start + 36], source_bytes);
    assert_eq!(root["extensionsUsed"], json!(["KHR_mesh_quantization"]));
    assert_eq!(root["extensionsRequired"], json!(["KHR_mesh_quantization"]));
}
#[test]
fn float_normals_are_normalized_and_zero_ones_repaired() {
    let mut model = Model::new();
    let position = model.positions(&[0., 0., 0., 1., 0., 0., 0., 1., 0.]);
    let normal = model.floats(&[0., 0., 2., 0., 0., 1., 0., 0., 0.], "VEC3", 3);
    model.primitive(json!({"attributes": {"POSITION": position, "NORMAL": normal}}));
    let (root, bin, stderr) = model.write().import(&[]);
    let id = primitive(&root)["attributes"]["NORMAL"].as_u64().unwrap();
    let normals = floats(&root, &bin, id);
    assert_eq!(normals[0], [0., 0., 1.]);
    assert_eq!(normals[1], [0., 0., 1.]);
    assert_eq!(normals[2], [0., 0., 1.]);
    assert!(
        stderr.contains("replaced 1 zero-length normals"),
        "{stderr}"
    );
}
#[test]
fn quantized_normals_are_left_as_they_are_and_counted() {
    let mut model = Model::new();
    let position = model.positions(&[0., 0., 0., 1., 0., 0., 0., 1., 0.]);
    let normal = model.data(&[0, 0, 127, 0, 0, 0, 0, 0, 0, 0, 100, 0], "VEC3", 5120, 3);
    model.root["accessors"][normal]["normalized"] = json!(true);
    model.root["bufferViews"][normal]["byteStride"] = json!(4);
    model.primitive(json!({"attributes": {"POSITION": position, "NORMAL": normal}}));
    model.root["extensionsUsed"] = json!(["KHR_mesh_quantization"]);
    let (root, bin, stderr) = model.write().import(&[]);
    let id = primitive(&root)["attributes"]["NORMAL"].as_u64().unwrap() as usize;
    let view = &root["bufferViews"][root["accessors"][id]["bufferView"].as_u64().unwrap() as usize];
    let start = view["byteOffset"].as_u64().unwrap() as usize;
    assert_eq!(
        bin[start..start + 12],
        [0, 0, 127, 0, 0, 0, 0, 0, 0, 0, 100, 0]
    );
    assert!(
        stderr.contains("1 quantized normals have zero length"),
        "{stderr}"
    );
}
#[test]
fn missing_normals_stay_missing_so_shading_stays_flat() {
    let (root, _, stderr) = Model::triangle().write().import(&[]);
    assert!(primitive(&root)["attributes"].get("NORMAL").is_none());
    assert!(!stderr.contains("normals"), "{stderr}");
}
#[test]
fn uvs_keep_their_direction_and_alpha_and_colours_stay_authored() {
    // A transparent PNG under an opaque material, and colours under a texture: both are meant.
    let (mut model, f) = Model::textured(
        json!({"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}, "alphaMode": "OPAQUE"}),
    );
    let mut rgba = image::RgbaImage::new(8, 8);
    rgba.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
    rgba.save(f.0.join("tex.png")).unwrap();
    let color = model.floats(&[1., 0., 0., 0., 1., 0., 0., 0., 1.], "VEC3", 3);
    model.root["meshes"][0]["primitives"][0]["attributes"]["COLOR_0"] = json!(color);
    model.write_to(&f);
    let (root, bin, stderr) = f.import(&[]);
    assert_eq!(root["materials"][0]["alphaMode"], "OPAQUE");
    let attributes = &primitive(&root)["attributes"];
    assert!(attributes.get("_COLOR").is_none());
    assert_eq!(
        floats(&root, &bin, attributes["COLOR_0"].as_u64().unwrap())[1],
        [0., 1., 0.]
    );
    assert_eq!(
        floats(&root, &bin, attributes["TEXCOORD_0"].as_u64().unwrap())[2],
        [0., 1.]
    );
    assert!(!stderr.contains("Warning"), "{stderr}");
    // `--no-color` drops them.
    fs::remove_file(f.0.join("out.gltf")).unwrap();
    fs::remove_file(f.0.join("out.bin")).unwrap();
    let (root, _, _) = f.import(&["--no-color"]);
    assert!(primitive(&root)["attributes"].get("COLOR_0").is_none());
}
#[test]
fn a_primitive_without_a_sampled_uv_set_gets_an_untextured_copy() {
    let material = json!({
        "name": "paint",
        "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}, "baseColorFactor": [1, 0, 0, 1]},
        "normalTexture": {"index": 0, "texCoord": 1},
        "occlusionTexture": {"index": 0, "extensions": {"KHR_texture_transform": {"texCoord": 1}}},
        "extensions": {"KHR_materials_clearcoat": {"clearcoatTexture": {"index": 0, "texCoord": 2}}},
    });
    let (model, f) = Model::textured(material);
    model.write_to(&f);
    let (root, _, _) = f.import(&[]);
    let material = &root["materials"][0];
    assert_eq!(material["name"], "paint-untextured");
    assert!(material["pbrMetallicRoughness"]
        .get("baseColorTexture")
        .is_some());
    assert_eq!(
        material["pbrMetallicRoughness"]["baseColorFactor"],
        json!([1, 0, 0, 1])
    );
    assert!(material.get("normalTexture").is_none());
    assert!(material.get("occlusionTexture").is_none());
    assert!(material["extensions"]["KHR_materials_clearcoat"]
        .get("clearcoatTexture")
        .is_none());
    assert_eq!(root["extensionsUsed"], json!(["KHR_materials_clearcoat"]));
    // The original survives for a primitive that has the sets.
    let (mut model, f) =
        Model::textured(json!({"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}));
    let position = model.root["meshes"][0]["primitives"][0]["attributes"]["POSITION"].clone();
    let indices = model.root["meshes"][0]["primitives"][0]["indices"].clone();
    model.primitive(
        json!({"attributes": {"POSITION": position}, "indices": indices, "material": 0}),
    );
    model.write_to(&f);
    let (root, _, _) = f.import(&[]);
    let list = root["meshes"][0]["primitives"].as_array().unwrap();
    assert_ne!(list[0]["material"], list[1]["material"]);
    assert_eq!(root["materials"].as_array().unwrap().len(), 2);
    assert_eq!(root["materials"][1]["name"], "material-untextured");
}
#[test]
fn only_what_is_used_is_published() {
    let (mut model, f) =
        Model::textured(json!({"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}));
    let materials = model.root["materials"].as_array_mut().unwrap();
    materials.insert(
        0,
        json!({"name": "unused", "pbrMetallicRoughness": {"baseColorTexture": {"index": 1}}}),
    );
    model.root["meshes"][0]["primitives"][0]["material"] = json!(1);
    model.root["textures"] = json!([{"source": 1}, {"source": 0}]);
    model.root["images"] = json!([{"uri": "unused.png"}, {"uri": "tex.png"}]);
    model.write_to(&f);
    let (root, _, _) = f.import(&[]);
    assert_eq!(root["materials"].as_array().unwrap().len(), 1);
    assert_eq!(root["textures"], json!([{"source": 0}]));
    assert_eq!(root["images"], json!([{"uri": "tex.png"}]));
    assert_eq!(primitive(&root)["material"], 0);
    assert_eq!(
        root["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"]["index"],
        0
    );
}
#[test]
fn material_variants_warn_and_only_the_default_material_stays() {
    let (mut model, f) = Model::textured(json!({"name": "default"}));
    let materials = model.root["materials"].as_array_mut().unwrap();
    materials.push(json!({
        "name": "variant",
        "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}},
    }));
    model.root["meshes"][0]["primitives"][0]["extensions"] = json!({
        "KHR_materials_variants": {"mappings": [{"material": 1, "variants": [0]}]},
    });
    model.root["extensions"] = json!({"KHR_materials_variants": {"variants": [{"name": "red"}]}});
    model.root["extensionsUsed"] = json!(["KHR_materials_variants"]);
    model.write_to(&f);
    let (root, _, stderr) = f.import(&[]);
    assert_eq!(root["materials"], json!([{"name": "default"}]));
    assert!(root.get("images").is_none(), "{root}");
    assert!(root.get("extensions").is_none(), "{root}");
    assert!(root.get("extensionsUsed").is_none(), "{root}");
    assert!(primitive(&root).get("extensions").is_none(), "{root}");
    assert!(
        stderr.contains(
            "Warning: material variants are not supported (KHR_materials_variants): \
             only each primitive's default material is used"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("KHR_materials_variants,"), "{stderr}");
    assert!(!stderr.contains("ignored"), "{stderr}");
}
#[test]
fn a_mime_type_contradicting_the_image_is_corrected_with_a_warning() {
    for (declared, expected, warns) in [
        (Some("image/jpeg"), Some("image/png"), true),
        (Some("image/png"), Some("image/png"), false),
        (None, None, false),
    ] {
        let (mut model, f) =
            Model::textured(json!({"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}));
        if let Some(declared) = declared {
            model.root["images"][0]["mimeType"] = json!(declared);
        }
        model.write_to(&f);
        let (root, _, stderr) = f.import(&[]);
        assert_eq!(root["images"][0]["mimeType"].as_str(), expected);
        assert_eq!(
            stderr.contains("Warning: corrected the mimeType of 1 images to match their content"),
            warns,
            "{stderr}"
        );
    }
}
#[test]
fn a_converted_image_declares_no_mime_type() {
    let (mut model, f) =
        Model::textured(json!({"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}));
    model.root["images"][0]["mimeType"] = json!("image/png");
    model.write_to(&f);
    let (root, _, stderr) = f.import(&["--convert-images", "true"]);
    assert!(root["images"][0].get("mimeType").is_none(), "{root}");
    assert!(!stderr.contains("mimeType"), "{stderr}");
}
#[test]
fn fully_transparent_materials_warn_only_when_blended() {
    let warns = |mode: &str| {
        let mut model = Model::triangle();
        model.root["meshes"][0]["primitives"][0]["material"] = json!(0);
        model.root["materials"] = json!([{"name": "ghost", "alphaMode": mode, "pbrMetallicRoughness": {"baseColorFactor": [1, 1, 1, 0]}}]);
        let (_, _, stderr) = model.write().import(&[]);
        stderr.contains("ghost is fully transparent")
    };
    assert!(warns("BLEND"));
    assert!(!warns("OPAQUE"));
}
#[test]
fn texture_transform_is_required_and_kept() {
    let transform = json!({"KHR_texture_transform": {"offset": [0.5, 0], "scale": [0.5, 1]}});
    let (mut model, f) = Model::textured(
        json!({"pbrMetallicRoughness": {"baseColorTexture": {"index": 0, "extensions": transform}}}),
    );
    model.root["extensionsUsed"] = json!(["KHR_texture_transform"]);
    model.write_to(&f);
    let (root, _, _) = f.import(&[]);
    assert_eq!(root["extensionsRequired"], json!(["KHR_texture_transform"]));
    assert_eq!(
        root["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"]["extensions"]
            ["KHR_texture_transform"]["scale"],
        json!([0.5, 1])
    );
}
#[test]
fn center_is_baked_into_positions_at_the_world_origin() {
    let mut model = Model::new();
    let position = model.positions(&[
        1000., 2000., 3000., 1002., 2000., 3000., 1000., 2004., 3000.,
    ]);
    model.primitive(json!({"attributes": {"POSITION": position}}));
    // A light moves by itself, under a node that carries nothing else.
    model.root["nodes"].as_array_mut().unwrap().push(
        json!({"translation": [1, 2, 3], "extensions": {"KHR_lights_punctual": {"light": 0}}}),
    );
    model.root["scenes"][0]["nodes"] = json!([0, 1]);
    model.root["extensions"] = json!({"KHR_lights_punctual": {"lights": [{"type": "point"}]}});
    model.root["extensionsUsed"] = json!(["KHR_lights_punctual"]);
    let f = model.write();
    let (root, bin, _) = f.import(&["--center"]);
    let id = primitive(&root)["attributes"]["POSITION"].as_u64().unwrap();
    assert_eq!(
        floats(&root, &bin, id),
        [[-1., -2., 0.], [1., -2., 0.], [-1., 2., 0.]]
    );
    assert_eq!(root["accessors"][id as usize]["min"], json!([-1., -2., 0.]));
    assert_eq!(root["accessors"][id as usize]["max"], json!([1., 2., 0.]));
    assert_eq!(
        root["extras"]["objtools"]["origin"],
        json!(["1001", "2002", "3000"])
    );
    assert_eq!(root["extras"]["objtools"]["centered"], true);
    assert_eq!(
        root["extras"]["objtools"]["sourceBounds"]["size"],
        json!(["2", "4", "0"])
    );
    assert_eq!(root["scenes"][0]["nodes"], json!([0, 1]));
    assert!(root["nodes"][0].get("translation").is_none());
    assert_eq!(
        root["nodes"][1]["translation"],
        json!([-1000., -2000., -2997.])
    );
}
/// The new parent's transform, with the node keeping its own and the positions their values.
fn assert_wrapped(root: &Value, bin: &[u8], translation: Value) {
    let top = root["scenes"][0]["nodes"][0].as_u64().unwrap() as usize;
    let wrapper = &root["nodes"][top];
    assert_eq!(wrapper["name"], "objtools");
    assert_eq!(wrapper["children"], json!([0]));
    assert_eq!(wrapper["translation"], translation);
    let id = primitive(root)["attributes"]["POSITION"].as_u64().unwrap();
    assert_eq!(floats(root, bin, id)[1], [1., 0., 0.]);
}
#[test]
fn center_uses_a_parent_node_when_meshes_are_not_at_the_origin() {
    // World x spans 10..11 and y 20..21, under the node's translation.
    let mut model = Model::triangle();
    model.root["nodes"][0]["translation"] = json!([10, 20, 0]);
    let (root, bin, _) = model.write().import(&["--center"]);
    assert_eq!(root["nodes"][0]["translation"], json!([10, 20, 0]));
    assert_eq!(
        root["extras"]["objtools"]["origin"],
        json!(["10.5", "20.5", "0"])
    );
    assert_wrapped(&root, &bin, json!([-10.5, -20.5, -0.]));
}
#[test]
fn center_measures_through_a_matrix_and_a_hierarchy() {
    let mut model = Model::triangle();
    model.root["nodes"] = json!([
        {"children": [1], "scale": [2, 2, 2]},
        {"mesh": 0, "matrix": [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 5, 0, 0, 1]},
    ]);
    let (root, _, _) = model.write().import(&["--center"]);
    // x: 2 · (5..6), y: 2 · (0..1).
    assert_eq!(
        root["extras"]["objtools"]["origin"],
        json!(["11", "1", "0"])
    );
    assert_eq!(root["nodes"][1]["matrix"][12], 5);
}
#[test]
fn center_uses_a_parent_node_for_animation_skins_quantization_and_shared_lights() {
    let wrapped = |root: &Value| {
        let top = root["scenes"][0]["nodes"][0].as_u64().unwrap() as usize;
        root["nodes"][top]["name"] == "objtools"
    };
    let mut model = Model::triangle();
    let time = model.floats(&[0., 1.], "SCALAR", 1);
    let value = model.floats(&[0., 0., 0., 1., 0., 0.], "VEC3", 3);
    model.root["accessors"][time]["min"] = json!([0.]);
    model.root["accessors"][time]["max"] = json!([1.]);
    model.root["animations"] = json!([{
        "samplers": [{"input": time, "output": value}],
        "channels": [{"sampler": 0, "target": {"node": 0, "path": "translation"}}],
    }]);
    let (root, bin, _) = model.write().import(&["--center"]);
    assert!(wrapped(&root));
    // The accessors moved with the animation.
    let sampler = &root["animations"][0]["samplers"][0];
    assert_eq!(
        floats(&root, &bin, sampler["output"].as_u64().unwrap())[1],
        [1., 0., 0.]
    );

    let mut model = Model::new();
    let position = model.data(&[0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0], "VEC3", 5121, 3);
    model.root["bufferViews"][position]["byteStride"] = json!(4);
    model.primitive(json!({"attributes": {"POSITION": position}}));
    model.root["extensionsUsed"] = json!(["KHR_mesh_quantization"]);
    assert!(wrapped(&model.write().import(&["--center"]).0));

    let mut model = Model::triangle();
    model.root["nodes"][0]["extensions"] = json!({"KHR_lights_punctual": {"light": 0}});
    model.root["extensions"] = json!({"KHR_lights_punctual": {"lights": [{"type": "point"}]}});
    model.root["extensionsUsed"] = json!(["KHR_lights_punctual"]);
    assert!(wrapped(&model.write().import(&["--center"]).0));
}
#[test]
fn a_skinned_mesh_stays_at_the_root_under_its_ignored_transform() {
    let mut model = Model::triangle();
    let matrices = model.data(&[0; 64], "MAT4", 5126, 1);
    model.root["nodes"] = json!([
        {"mesh": 0, "skin": 0, "translation": [100, 0, 0]},
        {"name": "joint"},
    ]);
    model.root["scenes"][0]["nodes"] = json!([0, 1]);
    model.root["skins"] = json!([{"joints": [1], "inverseBindMatrices": matrices}]);
    let (root, _, _) = model.write().import(&["--center"]);
    // The joint is what moves, and the bounds ignore the node's translation.
    assert_eq!(
        root["extras"]["objtools"]["origin"],
        json!(["0.5", "0.5", "0"])
    );
    let top = root["scenes"][0]["nodes"].as_array().unwrap();
    assert_eq!(top[0], 0);
    let wrapper = &root["nodes"][top[1].as_u64().unwrap() as usize];
    assert_eq!(wrapper["children"], json!([1]));
    assert_eq!(
        root["accessors"][root["skins"][0]["inverseBindMatrices"].as_u64().unwrap() as usize]
            ["type"],
        "MAT4"
    );
}
#[test]
fn instancing_is_kept_and_its_instances_count_in_the_bounds() {
    let mut model = Model::triangle();
    let translations = model.floats(&[0., 0., 0., 100., 0., 0.], "VEC3", 3);
    let scales = model.floats(&[1., 1., 1., 2., 2., 2.], "VEC3", 3);
    model.root["nodes"][0]["extensions"] = json!({"EXT_mesh_gpu_instancing": {"attributes": {"TRANSLATION": translations, "SCALE": scales}}});
    model.root["extensionsUsed"] = json!(["EXT_mesh_gpu_instancing"]);
    model.root["extensionsRequired"] = json!(["EXT_mesh_gpu_instancing"]);
    let (root, bin, _) = model.write().import(&["--center"]);
    // x: 0..1 and 100..102, y: 0..1 and 0..2.
    assert_eq!(
        root["extras"]["objtools"]["origin"],
        json!(["51", "1", "0"])
    );
    assert_eq!(
        root["extensionsRequired"],
        json!(["EXT_mesh_gpu_instancing"])
    );
    let attributes = &root["nodes"][0]["extensions"]["EXT_mesh_gpu_instancing"]["attributes"];
    assert_eq!(
        floats(&root, &bin, attributes["TRANSLATION"].as_u64().unwrap())[1],
        [100., 0., 0.]
    );
    assert_eq!(
        floats(&root, &bin, attributes["SCALE"].as_u64().unwrap())[1],
        [2., 2., 2.]
    );
}
#[test]
fn nothing_declares_an_extension_it_does_not_use() {
    let (root, _, _) = Model::triangle().write().import(&[]);
    assert!(root.get("extensionsUsed").is_none());
    assert!(root.get("extensionsRequired").is_none());
    assert!(root["nodes"][0].get("extensions").is_none());
}
#[test]
fn up_axis_rotates_at_the_scene_roots_after_the_center() {
    let mut model = Model::triangle();
    model.root["nodes"][0]["rotation"] = json!([0, 1, 0, 0]);
    model.root["nodes"][0]["translation"] = json!([10, 20, 30]);
    let (root, _, _) = model.write().import(&["--up-axis", "z"]);
    let top = root["scenes"][0]["nodes"][0].as_u64().unwrap() as usize;
    let q = std::f64::consts::FRAC_1_SQRT_2;
    assert_eq!(root["nodes"][top]["rotation"], json!([-q, 0., 0., q]));
    assert!(root["nodes"][top].get("translation").is_none());
    assert_eq!(root["nodes"][0]["rotation"], json!([0, 1, 0, 0]));
    assert_eq!(root["extras"]["objtools"]["sourceUpAxis"], "z");

    // With a center: rotated, (x, y, z) → (x, z, -y), so the new parent's translation is too.
    let mut model = Model::triangle();
    model.root["nodes"][0]["translation"] = json!([10, 20, 30]);
    let (root, _, _) = model.write().import(&["--up-axis", "z", "--center"]);
    let top = root["scenes"][0]["nodes"][0].as_u64().unwrap() as usize;
    assert_eq!(
        root["extras"]["objtools"]["origin"],
        json!(["10.5", "20.5", "30"])
    );
    let moved = root["nodes"][top]["translation"].as_array().unwrap();
    let expected = [-10.5, -30., 20.5];
    for (a, b) in moved.iter().zip(expected) {
        assert!((a.as_f64().unwrap() - b).abs() < 1e-9, "{moved:?}");
    }
}
#[test]
fn a_root_in_two_scenes_gets_a_parent_of_its_own() {
    let mut model = Model::triangle();
    model.root["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"mesh": 0, "translation": [5, 0, 0]}));
    model.root["scenes"] = json!([{"nodes": [0, 1]}, {"nodes": [0]}]);
    let (root, _, _) = model.write().import(&["--up-axis", "z"]);
    let first = root["scenes"][0]["nodes"].as_array().unwrap();
    let second = root["scenes"][1]["nodes"].as_array().unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 1);
    let parent_of = |child: u64| {
        let nodes = root["nodes"].as_array().unwrap();
        nodes
            .iter()
            .filter(|n| {
                n["children"]
                    .as_array()
                    .is_some_and(|c| c.contains(&json!(child)))
            })
            .count()
    };
    assert_eq!(parent_of(0), 1);
    assert_eq!(parent_of(1), 1);
    assert!(second.iter().all(|n| first.contains(n)));
}
#[test]
fn a_scene_is_created_when_the_input_has_none() {
    let mut model = Model::triangle();
    let map = model.root.as_object_mut().unwrap();
    map.remove("scene");
    map.remove("scenes");
    let (root, _, _) = model.write().import(&[]);
    assert_eq!(root["scene"], 0);
    assert_eq!(root["scenes"], json!([{"nodes": [0]}]));
}
#[test]
fn reimporting_adds_to_the_recorded_origin() {
    let mut model = Model::new();
    let position = model.positions(&[
        1000., 2000., 3000., 1002., 2000., 3000., 1000., 2004., 3000.,
    ]);
    model.primitive(json!({"attributes": {"POSITION": position}}));
    let f = model.write();
    let (first, _, _) = f.import(&["--center"]);
    assert_eq!(
        first["extras"]["objtools"]["origin"],
        json!(["1001", "2002", "3000"])
    );
    fs::rename(f.0.join("out.gltf"), f.0.join("in.gltf")).unwrap();
    fs::rename(f.0.join("out.bin"), f.0.join("in.bin")).unwrap();
    let text = fs::read_to_string(f.0.join("in.gltf"))
        .unwrap()
        .replace("out.bin", "in.bin");
    fs::write(f.0.join("in.gltf"), text).unwrap();
    // Already centered: nothing moves, and the origin and bounds survive.
    let (second, _, stderr) = f.import(&[]);
    assert_eq!(
        second["extras"]["objtools"]["origin"],
        json!(["1001", "2002", "3000"])
    );
    assert_eq!(second["extras"]["objtools"]["centered"], true);
    assert_eq!(
        second["extras"]["objtools"]["sourceBounds"],
        first["extras"]["objtools"]["sourceBounds"]
    );
    assert!(!stderr.contains("without recentering"), "{stderr}");
    // A new center adds to it.
    let mut moved: Value = serde_json::from_slice(&fs::read(f.0.join("in.gltf")).unwrap()).unwrap();
    moved["nodes"][0]["translation"] = json!([10, 0, 0]);
    fs::write(f.0.join("in.gltf"), moved.to_string()).unwrap();
    fs::remove_file(f.0.join("out.gltf")).unwrap();
    fs::remove_file(f.0.join("out.bin")).unwrap();
    let (third, _, _) = f.import(&["--center"]);
    assert_eq!(
        third["extras"]["objtools"]["origin"],
        json!(["1011", "2002", "3000"])
    );
}
#[test]
fn the_recorded_origin_stays_in_its_own_frame_under_a_rotation() {
    // As imported with `--up-axis z --center`: its node rotates source z to world y.
    let q = std::f64::consts::FRAC_1_SQRT_2;
    let mut model = Model::triangle();
    model.root["nodes"][0]["rotation"] = json!([-q, 0., 0., q]);
    model.root["extras"] = json!({"objtools": {"origin": ["100", "200", "300"], "centered": true, "sourceUpAxis": "z"}});
    let f = model.write();
    let (root, _, _) = f.import(&["--center"]);
    // Source y 0..1 is world z 0..-1: the center is (0.5, 0, -0.5) there and (0.5, 0.5, 0) in the
    // source frame. Rotation rounding leaves digits past the fixed-point grid's first few.
    let origin = root["extras"]["objtools"]["origin"].as_array().unwrap();
    let value = |i: usize| origin[i].as_str().unwrap().parse::<f64>().unwrap();
    for (i, expected) in [100.5, 200.5, 300.].into_iter().enumerate() {
        assert!((value(i) - expected).abs() < 1e-6, "{origin:?}");
    }
    assert_eq!(root["extras"]["objtools"]["sourceUpAxis"], "z");
    fs::remove_file(f.0.join("out.gltf")).unwrap();
    fs::remove_file(f.0.join("out.bin")).unwrap();
    f.fails(&["--up-axis", "z"], "already imported with up axis z");
}
#[test]
fn a_malformed_recorded_origin_is_an_error_rather_than_lost() {
    let mut model = Model::triangle();
    model.root["extras"] = json!({"objtools": {"origin": ["1", "2"]}});
    model.write().fails(&[], "Malformed extras.objtools.origin");
}
#[test]
fn unknown_extensions_are_dropped_or_refused() {
    let mut model = Model::triangle();
    model.root["nodes"][0]["extensions"] = json!({"VENDOR_kept_out": {"accessor": 0}});
    model.root["meshes"][0]["extras"] = json!({"extensions": {"VENDOR_data": 1}});
    model.root["extensionsUsed"] = json!(["VENDOR_kept_out"]);
    let (root, _, stderr) = model.write().import(&[]);
    assert!(root["nodes"][0].get("extensions").is_none());
    assert!(root.get("extensionsUsed").is_none());
    assert_eq!(root["meshes"][0]["extras"]["extensions"]["VENDOR_data"], 1);
    assert_eq!(stderr.matches("VENDOR_kept_out").count(), 1, "{stderr}");
    // One the file doesn't declare is dropped too, and said so.
    let mut model = Model::triangle();
    model.root["nodes"][0]["extensions"] = json!({"VENDOR_undeclared": {}});
    let (root, _, stderr) = model.write().import(&[]);
    assert!(root["nodes"][0].get("extensions").is_none());
    assert!(
        stderr.contains("dropped 1 extensions from the output: VENDOR_undeclared"),
        "{stderr}"
    );

    let mut model = Model::triangle();
    model.root["extensionsUsed"] = json!(["VENDOR_needed"]);
    model.root["extensionsRequired"] = json!(["VENDOR_needed"]);
    model.write().fails(&[], "VENDOR_needed");
}
#[test]
fn compression_is_refused_unless_uncompressed_data_is_there() {
    for name in [
        "KHR_draco_mesh_compression",
        "EXT_meshopt_compression",
        "KHR_meshopt_compression",
    ] {
        let mut model = Model::triangle();
        model.root["extensionsUsed"] = json!([name]);
        model.root["extensionsRequired"] = json!([name]);
        model.write().fails(&[], name);
    }
    // Optional Draco, with accessors that still hold the data.
    let mut model = Model::triangle();
    model.root["meshes"][0]["primitives"][0]["extensions"] =
        json!({"KHR_draco_mesh_compression": {"bufferView": 0, "attributes": {"POSITION": 0}}});
    model.root["extensionsUsed"] = json!(["KHR_draco_mesh_compression"]);
    let (root, bin, stderr) = model.write().import(&[]);
    assert!(primitive(&root).get("extensions").is_none());
    assert!(root.get("extensionsUsed").is_none());
    assert!(
        stderr.contains("dropped 1 extensions from the output: KHR_draco_mesh_compression"),
        "{stderr}"
    );
    assert_eq!(indices_of(&root, &bin, primitive(&root)), [0, 1, 2]);
    // Optional Draco without them, which must not read as zeros.
    let mut model = Model::triangle();
    model.root["meshes"][0]["primitives"][0]["extensions"] =
        json!({"KHR_draco_mesh_compression": {"bufferView": 0, "attributes": {"POSITION": 0}}});
    model.root["extensionsUsed"] = json!(["KHR_draco_mesh_compression"]);
    model.root["accessors"][0]
        .as_object_mut()
        .unwrap()
        .remove("bufferView");
    model.write().fails(&[], "no uncompressed data");
    // Optional meshopt, reading the fallback and leaving the compressed bytes behind.
    let mut model = Model::triangle();
    model.root["bufferViews"][0]["extensions"] = json!({"EXT_meshopt_compression": {"buffer": 0, "byteLength": 4, "byteStride": 12, "count": 3, "mode": "ATTRIBUTES"}});
    model.root["extensionsUsed"] = json!(["EXT_meshopt_compression"]);
    let (root, bin, _) = model.write().import(&[]);
    assert!(root["bufferViews"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v.get("extensions").is_none()));
    let id = primitive(&root)["attributes"]["POSITION"].as_u64().unwrap();
    assert_eq!(floats(&root, &bin, id)[1], [1., 0., 0.]);
}
/// A KTX2 header, or the AVIF boxes leading to its size; nothing past them is read.
fn ktx2() -> Vec<u8> {
    let mut out = b"\xabKTX 20\xbb\r\n\x1a\n".to_vec();
    for field in [0u32, 1, 16, 16] {
        out.extend(field.to_le_bytes());
    }
    out
}
fn avif() -> Vec<u8> {
    let boxed = |tag: &[u8; 4], body: &[u8]| {
        let mut out = (body.len() as u32 + 8).to_be_bytes().to_vec();
        out.extend(tag);
        out.extend(body);
        out
    };
    let mut ispe = vec![0; 4];
    ispe.extend(16u32.to_be_bytes());
    ispe.extend(16u32.to_be_bytes());
    let mut meta = vec![0; 4];
    meta.extend(boxed(b"iprp", &boxed(b"ipco", &boxed(b"ispe", &ispe))));
    let mut out = boxed(b"ftyp", b"avif\0\0\0\0avif");
    out.extend(boxed(b"meta", &meta));
    out
}
/// A texture selecting `file` through `extension`, with `tex.png` as its fallback if asked.
fn extension_image(extension: &str, file: &str, fallback: bool) -> Fixture {
    let (mut model, f) =
        Model::textured(json!({"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}));
    model.root["images"]
        .as_array_mut()
        .unwrap()
        .push(json!({"uri": file}));
    model.root["textures"][0]["extensions"] = json!({extension: {"source": 1}});
    model.root["extensionsUsed"] = json!([extension]);
    if !fallback {
        model.root["textures"][0]
            .as_object_mut()
            .unwrap()
            .remove("source");
        model.root["images"] = json!([{"uri": "tex.png"}, {"uri": file}]);
        model.root["extensionsRequired"] = json!([extension]);
    }
    match file.rsplit('.').next().unwrap() {
        "webp" => image::RgbImage::new(16, 16).save(f.0.join(file)).unwrap(),
        "ktx2" => fs::write(f.0.join(file), ktx2()).unwrap(),
        _ => fs::write(f.0.join(file), avif()).unwrap(),
    }
    model.write_to(&f);
    f
}
#[test]
fn extension_images_are_kept_with_their_extension() {
    for (extension, file) in [
        ("EXT_texture_webp", "extra.webp"),
        ("KHR_texture_basisu", "extra.ktx2"),
        ("EXT_texture_avif", "extra.avif"),
    ] {
        for args in [&[][..], &["--convert-images", "false"]] {
            let f = extension_image(extension, file, true);
            let (root, _, _) = f.import(args);
            assert_eq!(root["textures"][0]["source"], 0, "{extension}");
            assert_eq!(
                root["textures"][0]["extensions"][extension]["source"], 1,
                "{extension}"
            );
            assert_eq!(root["images"][0]["uri"], "tex.png", "{extension}");
            assert_eq!(root["images"][1]["uri"], file, "{extension}");
            assert_eq!(root["extensionsUsed"], json!([extension]), "{extension}");
            assert!(root.get("extensionsRequired").is_none(), "{extension}");
            assert_eq!(
                inspect(&f, "out.gltf")["diffuse_texels"],
                256,
                "{extension}"
            );
        }
        let f = extension_image(extension, file, false);
        let (root, _, _) = f.import(&[]);
        assert_eq!(
            root["extensionsRequired"],
            json!([extension]),
            "{extension}"
        );
        assert!(root["textures"][0].get("source").is_none(), "{extension}");
        assert_eq!(root["images"].as_array().unwrap().len(), 1, "{extension}");
        assert_eq!(root["images"][0]["uri"], file, "{extension}");
        assert_eq!(
            root["textures"][0]["extensions"][extension]["source"], 0,
            "{extension}"
        );
    }
}
#[test]
fn converting_replaces_a_webp_extension_and_its_fallback() {
    let f = extension_image("EXT_texture_webp", "extra.webp", true);
    let (root, _, _) = f.import(&["--convert-images", "true", "--image-format", "png"]);
    assert_eq!(root["textures"], json!([{"source": 0}]));
    assert_eq!(root["images"], json!([{"uri": "out_texture_0.png"}]));
    assert!(root.get("extensionsUsed").is_none());
    let image = image::open(f.0.join("out_texture_0.png")).unwrap();
    assert_eq!((image.width(), image.height()), (16, 16));
}
#[test]
fn avif_and_ktx2_cannot_be_converted_in_this_build() {
    for (extension, file) in [
        ("KHR_texture_basisu", "extra.ktx2"),
        ("EXT_texture_avif", "extra.avif"),
    ] {
        extension_image(extension, file, true)
            .fails(&["--convert-images", "true"], "can't be converted");
    }
}
#[test]
fn a_format_only_an_extension_makes_valid_follows_the_obj_rules_without_it() {
    let (mut model, f) =
        Model::textured(json!({"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}));
    model.root["images"] = json!([{"uri": "plain.webp"}]);
    image::RgbImage::new(16, 16)
        .save(f.0.join("plain.webp"))
        .unwrap();
    model.write_to(&f);
    f.fails(&["--convert-images", "false"], "--convert-images=false");
    let (root, _, _) = f.import(&[]);
    assert_eq!(root["images"][0]["uri"], "out_texture_0.jpg");
    assert_eq!(
        &fs::read(f.0.join("out_texture_0.jpg")).unwrap()[..2],
        [0xff, 0xd8]
    );
    // And AVIF, which can't be decoded, can't take that route.
    let (mut model, f) =
        Model::textured(json!({"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}));
    model.root["images"] = json!([{"uri": "plain.avif"}]);
    fs::write(f.0.join("plain.avif"), avif()).unwrap();
    model.write_to(&f);
    f.fails(&[], "can't be converted");
}
#[test]
fn image_policies_follow_the_options() {
    let (model, f) =
        Model::textured(json!({"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}));
    model.write_to(&f);
    let (root, _, _) = f.import(&["--images", "copy"]);
    assert_eq!(root["images"][0]["uri"], "out_texture_0.png");
    assert!(f.entries().contains(&"out_texture_0.png".to_owned()));
    // In another folder the default is a copy, and `keep` a relative reference.
    fs::create_dir(f.0.join("elsewhere")).unwrap();
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_objtools"))
            .arg("import")
            .arg(f.0.join("in.gltf"))
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let target = f.0.join("elsewhere/a.gltf");
    run(&["-o", target.to_str().unwrap()]);
    let root: Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
    assert_eq!(root["images"][0]["uri"], "a_texture_0.png");
    let target = f.0.join("elsewhere/b.gltf");
    run(&["-o", target.to_str().unwrap(), "--images", "keep"]);
    let root: Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
    assert_eq!(root["images"][0]["uri"], "../tex.png");
}
#[test]
fn broken_and_missing_images_are_errors() {
    let (model, f) =
        Model::textured(json!({"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}));
    fs::write(f.0.join("tex.png"), b"\x89PNG not really").unwrap();
    model.write_to(&f);
    f.fails(&[], "Texture");
    let (model, f) =
        Model::textured(json!({"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}));
    fs::remove_file(f.0.join("tex.png")).unwrap();
    model.write_to(&f);
    f.fails(&[], "Texture");
}
#[test]
fn an_imported_obj_survives_a_second_import_unchanged_in_its_statistics() {
    let f = Fixture::new();
    image::RgbImage::new(8, 8)
        .save(f.0.join("tex.png"))
        .unwrap();
    fs::write(f.0.join("first.mtl"), "newmtl paint\nmap_Kd tex.png\n").unwrap();
    fs::write(f.0.join("first.obj"), "mtllib first.mtl\nv 10 20 30\nv 11 20 30\nv 11 21 30\nv 10 21 30\nvt .25 .25\nvt .5 .25\nvt .5 .75\nvt .25 .75\nusemtl paint\nf 1/1 2/2 3/3 4/4\n").unwrap();
    let first = Command::new(env!("CARGO_BIN_EXE_objtools"))
        .arg("import")
        .arg(f.0.join("first.obj"))
        .args(["--center", "--up-axis", "z"])
        .output()
        .unwrap();
    assert!(first.status.success());
    let before = inspect(&f, "first.gltf");
    let out = f.run("first.gltf", &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(inspect(&f, "out.gltf"), before);
    let source: Value = serde_json::from_slice(&fs::read(f.0.join("first.gltf")).unwrap()).unwrap();
    let again: Value = serde_json::from_slice(&fs::read(f.0.join("out.gltf")).unwrap()).unwrap();
    assert_eq!(
        again["extras"]["objtools"]["origin"],
        source["extras"]["objtools"]["origin"]
    );
    assert_eq!(again["extras"]["objtools"]["sourceUpAxis"], "z");
    assert_eq!(
        again["materials"][0]["extras"],
        source["materials"][0]["extras"]
    );
    assert_eq!(again["extensionsUsed"], source["extensionsUsed"]);
}
