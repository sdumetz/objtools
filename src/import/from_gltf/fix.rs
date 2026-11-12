//! Changes that make the output meet the contract in the module's documentation. Each function
//! names what holds once it has run, and counts what it had to change in a warning.
use super::{
    payload::{Output, Patches, Payload},
    Options,
};
use crate::{
    import::{
        gltf::SHORT_VERTICES,
        images::{self, GltfImage, Kind, Plan},
        triangulate,
    },
    shared::{
        gltf::{element, extensions, index, Accessor, Gltf, Image},
        Res,
    },
};
use serde_json::{json, Value};
use std::{
    collections::{btree_map::Entry, hash_map, BTreeMap, BTreeSet, HashMap},
    fs,
    io::Write,
    path::Path,
};
const VEC3_FLOAT: (usize, u64) = (3, 5126);
/// The corners of triangle `t`, in the order the spec gives for each topology.
fn corners(mode: u64, t: usize) -> [usize; 3] {
    match mode {
        4 => [3 * t, 3 * t + 1, 3 * t + 2],
        5 => [t, t + 1 + t % 2, t + 2 - t % 2],
        _ => [t + 1, t + 2, 0],
    }
}
fn triangle_count(mode: u64, count: usize) -> usize {
    if mode == 4 {
        count / 3
    } else {
        count.saturating_sub(2)
    }
}
/// The vertices of each triangle of a primitive, however it is stored.
fn triangles<'a>(
    mode: u64,
    vertices: usize,
    source: Option<&'a Accessor>,
) -> impl Iterator<Item = [usize; 3]> + 'a {
    let count = source.map_or(vertices, |a| a.count);
    (0..triangle_count(mode, count))
        .map(move |t| corners(mode, t).map(|k| source.map_or(k, |a| a.get(k, 0) as usize)))
}
fn point(a: &Accessor, i: usize) -> [f64; 3] {
    std::array::from_fn(|c| a.get(i, c))
}
fn triangle(a: &Accessor, [i, j, k]: [usize; 3]) -> [[f64; 3]; 3] {
    [point(a, i), point(a, j), point(a, k)]
}
/// What rewriting one primitive's indices produced.
#[derive(Clone, Copy, Default)]
struct Faces {
    accessor: Option<usize>,
    written: u64,
    degenerate: u64,
}
/// Establishes that the primitive is an indexed triangle list without zero-area triangles, using
/// the smallest index type its vertex count allows. The vertices stay in place: other primitives
/// and morph targets may use them.
fn triangle_list_indices(
    payload: &mut Payload,
    positions: &Accessor,
    source: Option<&Accessor>,
    mode: u64,
) -> Res<Faces> {
    let short = positions.count as u64 <= SHORT_VERTICES;
    payload.pad(0)?;
    let start = payload.offset;
    let mut faces = Faces::default();
    for face in triangles(mode, positions.count, source) {
        if !triangulate::has_area(&triangle(positions, face), [0, 1, 2]) {
            faces.degenerate += 1;
            continue;
        }
        for i in face {
            if short {
                payload.out.write_all(&(i as u16).to_le_bytes())?;
            } else {
                payload.out.write_all(&(i as u32).to_le_bytes())?;
            }
        }
        faces.written += 1;
    }
    if faces.written > 0 {
        let bytes = faces.written * 3 * if short { 2 } else { 4 };
        payload.offset += bytes;
        payload
            .views
            .push(json!({"buffer":0,"byteOffset":start,"byteLength":bytes,"target":34963}));
        let component = if short { 5123 } else { 5125 };
        payload
            .accessors
            .push(json!({"bufferView":payload.views.len() - 1,
            "componentType":component,"count":faces.written * 3,"type":"SCALAR"}));
        faces.accessor = Some(payload.accessors.len() - 1);
    }
    Ok(faces)
}
/// A `POSITION` accessor's range in values as the renderer reads them, for bounds.
pub struct PositionRange {
    /// The accessor in the source.
    pub old: usize,
    pub min: [f64; 3],
    pub max: [f64; 3],
}
/// Establishes that `POSITION` bounds match the data. The spec defines them on values as stored,
/// so a quantized accessor gets integers.
fn position_bounds(old: usize, positions: &Accessor) -> (PositionRange, [Value; 2]) {
    let mut range = PositionRange {
        old,
        min: [f64::INFINITY; 3],
        max: [f64::NEG_INFINITY; 3],
    };
    let (mut low, mut high) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for i in 0..positions.count {
        for c in 0..3 {
            let (value, raw) = (positions.get(i, c), positions.raw(i, c));
            range.min[c] = range.min[c].min(value);
            range.max[c] = range.max[c].max(value);
            low[c] = low[c].min(raw);
            high[c] = high[c].max(raw);
        }
    }
    let encode = |v: [f64; 3]| {
        if positions.kind == 5126 {
            json!(v.map(|x| x as f32))
        } else {
            json!(v.map(|x| x as i64))
        }
    };
    (range, [encode(low), encode(high)])
}
/// Visits each texture slot of a material, wherever an extension puts it, and removes those the
/// visitor rejects. Every slot in the specification and the supported extensions ends in
/// `Texture`.
pub fn texture_slots(value: &mut Value, visit: &mut impl FnMut(&mut Value) -> bool) {
    if let Some(map) = value.as_object_mut() {
        map.retain(|key, child| {
            if key == "extras" {
                return true;
            }
            if key.ends_with("Texture") && child.get("index").is_some() {
                return visit(child);
            }
            texture_slots(child, visit);
            true
        });
    }
}
/// The UV set a slot samples. `KHR_texture_transform` overrides `texCoord`, as its spec says.
fn slot_set(slot: &Value) -> u64 {
    let transform = &slot["extensions"]["KHR_texture_transform"]["texCoord"];
    let own = slot["texCoord"].as_u64();
    transform.as_u64().or(own).unwrap_or(0)
}
/// Materials as primitives use them, which leaves out those none uses.
pub struct Materials {
    pub source: Vec<Value>,
    pub output: Vec<Value>,
    /// By source material and the UV sets it samples and its primitive lacks.
    pub ids: BTreeMap<(usize, Vec<u64>), usize>,
}
impl Materials {
    pub fn new(source: Vec<Value>) -> Self {
        Self {
            source,
            output: Vec::new(),
            ids: BTreeMap::new(),
        }
    }
    /// Establishes that a primitive has every UV set its material samples: if it lacks one, it
    /// gets a copy without those textures, as in obj2gltf's `splitIncompatibleMaterials`. glTF
    /// forbids the reference, and the colour factors still apply.
    fn with_uv_sets_of(&mut self, source: usize, attributes: &Value) -> usize {
        let material = &self.source[source];
        let mut sets = BTreeSet::new();
        texture_slots(&mut material.clone(), &mut |slot| {
            sets.insert(slot_set(slot));
            true
        });
        let lacks = |set: &u64| attributes.get(format!("TEXCOORD_{set}")).is_none();
        let missing: Vec<u64> = sets.into_iter().filter(lacks).collect();
        if let Some(id) = self.ids.get(&(source, missing.clone())) {
            return *id;
        }
        let mut value = material.clone();
        if !missing.is_empty() {
            let name = material["name"].as_str().unwrap_or("material");
            texture_slots(&mut value, &mut |slot| !missing.contains(&slot_set(slot)));
            value["name"] = json!(format!("{name}-untextured"));
        }
        self.output.push(value);
        let id = self.output.len() - 1;
        self.ids.insert((source, missing), id);
        id
    }
}
/// A primitive that survived, as it read in the source.
pub struct Kept {
    mode: u64,
    position: usize,
    indices: Option<usize>,
    normal: Option<usize>,
}
/// The rewritten meshes and what later steps need to know about them.
pub struct Geometry {
    pub meshes: Vec<Value>,
    /// New index of each source mesh, unless it lost all its primitives.
    pub mesh_ids: Vec<Option<usize>>,
    /// By the output's accessor id.
    pub ranges: BTreeMap<usize, PositionRange>,
    pub kept: Vec<Kept>,
    pub faces: u64,
}
/// Rewrites every primitive to the contract's geometry and material rules, copying the accessors
/// it uses. A primitive left without triangles is omitted, and a mesh left without primitives.
pub fn meshes(
    gltf: &Gltf,
    payload: &mut Payload,
    meshes: Vec<Value>,
    materials: &mut Materials,
) -> Res<Geometry> {
    // Primitives sharing all three share one result, and stay sharing one accessor.
    let mut shared: HashMap<(Option<usize>, usize, u64), Faces> = HashMap::new();
    let mut geometry = Geometry {
        meshes: Vec::new(),
        mesh_ids: vec![None; meshes.len()],
        ranges: BTreeMap::new(),
        kept: Vec::new(),
        faces: 0,
    };
    let mut degenerate = 0u64;
    for (mesh_id, mut mesh) in meshes.into_iter().enumerate() {
        let mut primitives = Vec::new();
        for mut primitive in super::take_array(&mut mesh, "primitives") {
            let mode = primitive["mode"].as_u64().unwrap_or(4);
            let position = index(&primitive["attributes"]["POSITION"], "POSITION accessor")?;
            let reader = Accessor::new(gltf, position)?;
            let indices = primitive.get("indices");
            let indices = indices.map(|id| index(id, "indices")).transpose()?;
            let faces = match shared.entry((indices, position, mode)) {
                hash_map::Entry::Occupied(entry) => *entry.get(),
                hash_map::Entry::Vacant(slot) => {
                    let source = indices.map(|id| Accessor::new(gltf, id)).transpose()?;
                    let faces = triangle_list_indices(payload, &reader, source.as_ref(), mode)?;
                    *slot.insert(faces)
                }
            };
            // Per primitive, like `geometry.faces`: each one loses these faces, shared or not.
            degenerate += faces.degenerate;
            let Some(new_indices) = faces.accessor else {
                continue;
            };
            geometry.faces += faces.written;
            let new = payload.accessor(position)?;
            if let Entry::Vacant(slot) = geometry.ranges.entry(new) {
                let (range, [min, max]) = position_bounds(position, &reader);
                slot.insert(range);
                payload.accessors[new]["min"] = min;
                payload.accessors[new]["max"] = max;
            }
            let normal = primitive["attributes"].get("NORMAL");
            geometry.kept.push(Kept {
                mode,
                position,
                indices,
                normal: normal.map(|id| index(id, "NORMAL")).transpose()?,
            });
            if let Some(id) = primitive.get("material") {
                let id = index(id, "material")?;
                let id = materials.with_uv_sets_of(id, &primitive["attributes"]);
                primitive["material"] = json!(id);
            }
            payload.attributes(&mut primitive["attributes"])?;
            for target in primitive
                .get_mut("targets")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                payload.attributes(target)?;
            }
            primitive["indices"] = json!(new_indices);
            primitive["mode"] = json!(4);
            primitives.push(primitive);
        }
        if !primitives.is_empty() {
            mesh["primitives"] = json!(primitives);
            geometry.mesh_ids[mesh_id] = Some(geometry.meshes.len());
            geometry.meshes.push(mesh);
        }
    }
    if geometry.faces == 0 {
        return Err("Input contains no supported triangle geometry".into());
    }
    if degenerate > 0 {
        crate::say!("Warning: skipped {degenerate} faces with zero area");
    }
    Ok(geometry)
}
/// Establishes that the model has a scene to show, as OBJ output always does.
pub fn missing_scene(scenes: &mut Vec<Value>, parent_of: &[Option<usize>]) {
    if scenes.is_empty() {
        let roots: Vec<usize> = (0..parent_of.len())
            .filter(|n| parent_of[*n].is_none())
            .collect();
        scenes.push(json!({"nodes": roots}));
    }
}
/// Establishes that float normals used by a triangle have unit length. One with zero length takes
/// the normal of a face using it: unlike the OBJ path there is no shared position index to smooth
/// over. Quantized normals can't be made exactly unit length, so they are only counted.
pub fn normals(gltf: &Gltf, map: &mut Patches, output: &Output, kept: &[Kept]) -> Res<()> {
    let normals: BTreeSet<usize> = kept.iter().filter_map(|k| k.normal).collect();
    let mut zero = BTreeSet::new();
    let (mut repaired, mut quantized) = (0u64, 0u64);
    for &old in &normals {
        let a = Accessor::new(gltf, old)?;
        let patchable = (a.components, a.kind) == VEC3_FLOAT && a.is_dense();
        let (start, stride) = if patchable {
            output.location(output.accessor_ids[&old])
        } else {
            (0, 0)
        };
        for i in 0..a.count {
            let n = point(&a, i);
            let squared: f64 = n.iter().map(|x| x * x).sum();
            if squared == 0. {
                if patchable {
                    zero.insert(old);
                } else {
                    quantized += 1;
                }
            } else if patchable && (squared - 1.).abs() > 1e-4 {
                let length = squared.sqrt();
                map.write_vec3(start + i * stride, n.map(|x| x / length));
            }
        }
    }
    for k in kept {
        let Some(normal) = k.normal.filter(|n| zero.contains(n)) else {
            continue;
        };
        let source = k.indices.map(|id| Accessor::new(gltf, id)).transpose()?;
        let positions = Accessor::new(gltf, k.position)?;
        let (start, stride) = output.location(output.accessor_ids[&normal]);
        for face in triangles(k.mode, positions.count, source.as_ref()) {
            let [a, b, c] = triangle(&positions, face);
            let Some(flat) = triangulate::face_normal(a, b, c) else {
                continue;
            };
            for v in face {
                if map.read_vec3(start + v * stride) == [0.; 3] {
                    map.write_vec3(start + v * stride, flat);
                    repaired += 1;
                }
            }
        }
    }
    if repaired > 0 {
        crate::say!(
            "Warning: replaced {repaired} zero-length normals with the normal of a face using them"
        );
    }
    if quantized > 0 {
        crate::say!(
            "Warning: {quantized} quantized normals have zero length and were left as they are"
        );
    }
    Ok(())
}
/// Establishes that nothing stays embedded and that each image is in a format the output allows,
/// renumbering the images the textures use. An embedded one is written out as a file.
pub fn plan_images(
    gltf: &Gltf,
    textures: &mut [Value],
    opts: &Options,
    workspace: &Path,
) -> Res<(Vec<Value>, Vec<Plan>)> {
    let mut ids: BTreeMap<usize, usize> = BTreeMap::new();
    let mut under_extension = BTreeSet::new();
    let mut renumber = |id: &Value| -> Res<usize> {
        let old = index(id, "texture source image")?;
        let next = ids.len();
        Ok(*ids.entry(old).or_insert(next))
    };
    for texture in textures.iter_mut() {
        if !texture["source"].is_null() {
            texture["source"] = json!(renumber(&texture["source"])?);
        }
        for name in extensions::IMAGE_EXTENSIONS {
            let source = texture["extensions"][name]["source"].clone();
            if !source.is_null() {
                under_extension.insert(index(&source, "texture source image")?);
                texture["extensions"][name]["source"] = json!(renumber(&source)?);
            }
        }
    }
    let mut images = vec![Value::Null; ids.len()];
    let mut plans = Vec::new();
    let mut corrected = 0u64;
    let mut composited = images::composited_tally();
    for (&old, &new) in &ids {
        let mut image = element(&gltf.root, "images", old)?.clone();
        let (path, embedded) = match gltf.image(old)? {
            Image::File(path) => (path, false),
            Image::Bytes(bytes) => {
                let extension = Kind::of(bytes).extension().unwrap_or("bin");
                let file = workspace.join(format!("embedded-{old}.{extension}"));
                fs::write(&file, bytes)?;
                (file, true)
            }
        };
        let plan = images::prepare_gltf_image(
            &opts.images,
            &GltfImage {
                path: &path,
                embedded,
                extension: under_extension.contains(&old),
            },
            new,
            &opts.input,
            &opts.output,
            workspace,
            &mut composited,
        )?;
        if let Some(map) = image.as_object_mut() {
            map.remove("bufferView");
            match (plan.mime, map.get_mut("mimeType")) {
                (None, _) => {
                    map.remove("mimeType");
                }
                (Some(real), Some(declared)) if declared != real => {
                    *declared = json!(real);
                    corrected += 1;
                }
                _ => {}
            }
        }
        image["uri"] = json!(plan.uri);
        images[new] = image;
        plans.push(plan);
    }
    if corrected > 0 {
        crate::say!("Warning: corrected the mimeType of {corrected} images to match their content");
    }
    Ok((images, plans))
}
fn used_extensions(value: &Value, used: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "extras" {
                    continue;
                }
                if key == "extensions" {
                    let list = child.as_object().into_iter();
                    used.extend(list.flat_map(|e| e.keys().cloned()));
                }
                used_extensions(child, used);
            }
        }
        Value::Array(list) => list.iter().for_each(|v| used_extensions(v, used)),
        _ => {}
    }
}
/// Establishes that `extensionsUsed` lists exactly what the document uses, and that
/// `extensionsRequired` keeps what the input required of those. Quantization has no JSON of its
/// own to find. Without the transform a texture lands in the wrong place, as in the OBJ path.
pub fn extension_lists(root: &mut Value, declared: &BTreeSet<String>, required: &BTreeSet<String>) {
    let map = root.as_object_mut().unwrap();
    map.remove("extensionsUsed");
    map.remove("extensionsRequired");
    let mut used = BTreeSet::new();
    used_extensions(root, &mut used);
    if declared.contains("KHR_mesh_quantization") {
        used.insert("KHR_mesh_quantization".to_owned());
    }
    let needed: Vec<&String> = used
        .iter()
        .filter(|name| required.contains(*name) || *name == "KHR_texture_transform")
        .collect();
    if !needed.is_empty() {
        root["extensionsRequired"] = json!(needed);
    }
    if !used.is_empty() {
        root["extensionsUsed"] = json!(used);
    }
}
