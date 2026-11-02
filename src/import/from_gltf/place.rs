//! `--center` and `--up-axis`: where the model ends up, and what the output records about it.
use super::{
    fix::{Geometry, PositionRange},
    matrix::{
        apply, instance_count, instance_matrix, instancing, local_matrix, multiply, numbers, trs,
        IDENTITY,
    },
    payload::{Output, Patches},
    Model, Options,
};
use crate::{
    import::fixed::{format_fixed_as_decimal, format_triple, parse_decimal_fixed, BBox, SCALE},
    shared::{
        gltf::{index, Accessor, Gltf},
        Res,
    },
};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
/// What an earlier objtools import recorded about this file. A new center adds to its origin, in
/// the frame that origin is in, whatever rotation the nodes now carry.
pub struct Previous {
    origin: [i64; 3],
    pub z_up: bool,
    pub centered: bool,
}
impl Previous {
    /// A malformed origin is an error rather than a lost georeference.
    pub fn read(extras: &Value) -> Res<Self> {
        let old = &extras["objtools"];
        let origin = match old.get("origin") {
            None | Some(Value::Null) => [0; 3],
            Some(origin) => {
                let malformed = "Malformed extras.objtools.origin";
                let list = origin
                    .as_array()
                    .filter(|a| a.len() == 3)
                    .ok_or(malformed)?;
                let part = |i: usize| parse_decimal_fixed(list[i].as_str().ok_or(malformed)?);
                [part(0)?, part(1)?, part(2)?]
            }
        };
        Ok(Self {
            origin,
            z_up: old["sourceUpAxis"] == "z",
            centered: old["centered"].as_bool().unwrap_or(false),
        })
    }
    /// A point in the current world frame, as source coordinates.
    fn to_source(&self, [x, y, z]: [i64; 3]) -> Res<[i64; 3]> {
        let rotated = if self.z_up {
            [x, z.checked_neg().ok_or("Coordinate out of range")?, y]
        } else {
            [x, y, z]
        };
        let sum = |i: usize| {
            self.origin[i]
                .checked_add(rotated[i])
                .ok_or("Origin out of range")
        };
        Ok([sum(0)?, sum(1)?, sum(2)?])
    }
}
/// `f64` holds any float32 exactly, so its shortest decimal text reaches the fixed-point path
/// with every digit the source had.
fn fixed(v: f64) -> Res<i64> {
    parse_decimal_fixed(&v.to_string())
}
/// Adds a box seen through `m`. Under a rotation this can be slightly larger than the geometry,
/// which only moves the center: reading every vertex once per instance wouldn't scale.
fn add_box(bounds: &mut BBox, m: &[f64; 16], range: &PositionRange) -> Res<()> {
    for corner in 0..8 {
        let p = std::array::from_fn(|c| {
            if corner >> c & 1 == 0 {
                range.min[c]
            } else {
                range.max[c]
            }
        });
        let world = apply(m, p);
        bounds.add([fixed(world[0])?, fixed(world[1])?, fixed(world[2])?]);
    }
    Ok(())
}
/// The bounds of everything the scenes show, in world space. The spec ignores a skinned mesh's
/// node, so it counts as stored: its bind pose, not any animated one.
fn world_bounds(gltf: &Gltf, model: &Model, geometry: &Geometry) -> Res<BBox> {
    let nodes = &model.nodes;
    let mut bounds = BBox::new();
    let mut visited = BTreeSet::new();
    for root in model
        .scenes
        .iter()
        .flat_map(|s| s["nodes"].as_array().into_iter().flatten())
    {
        let root = index(root, "scene node")?;
        if !visited.insert(root) {
            continue;
        }
        let mut stack = vec![(root, local_matrix(&nodes[root])?)];
        while let Some((id, world)) = stack.pop() {
            let node = &nodes[id];
            if let Some(mesh) = node.get("mesh") {
                let mesh = &geometry.meshes[index(mesh, "mesh")?];
                let instances = instancing(gltf, node)?;
                let base = if node.get("skin").is_some() {
                    IDENTITY
                } else {
                    world
                };
                for primitive in mesh["primitives"].as_array().unwrap() {
                    let position = index(&primitive["attributes"]["POSITION"], "POSITION")?;
                    let range = &geometry.ranges[&position];
                    let Some(attributes) = &instances else {
                        add_box(&mut bounds, &base, range)?;
                        continue;
                    };
                    for i in 0..instance_count(attributes) {
                        let m = multiply(&base, &instance_matrix(attributes, i));
                        add_box(&mut bounds, &m, range)?;
                    }
                }
            }
            for child in node["children"].as_array().into_iter().flatten() {
                let child = index(child, "child node")?;
                stack.push((child, multiply(&world, &local_matrix(&nodes[child])?)));
            }
        }
    }
    Ok(bounds)
}
/// Whether every mesh sits at the world origin with nothing else moving with it, so `--center` can
/// go into `POSITION` itself, as in the OBJ path, which keeps precision in a float32 renderer.
/// Otherwise a new parent node carries the translation, for the whole model alike.
fn meshes_can_take_the_center(
    gltf: &Gltf,
    model: &Model,
    parent_of: &[Option<usize>],
    geometry: &Geometry,
) -> Res<bool> {
    if !model.skins.is_empty() || !model.animations.is_empty() {
        return Ok(false);
    }
    for range in geometry.ranges.values() {
        let a = Accessor::new(gltf, range.old)?;
        if (a.components, a.kind) != (3, 5126) || !a.is_dense() {
            return Ok(false);
        }
    }
    for (id, node) in model.nodes.iter().enumerate() {
        if node.get("mesh").is_none() {
            continue;
        }
        if !node["extensions"]["EXT_mesh_gpu_instancing"].is_null() {
            return Ok(false);
        }
        let mut current = Some(id);
        while let Some(at) = current {
            let carrier = &model.nodes[at];
            if local_matrix(carrier)? != IDENTITY
                || carrier.get("camera").is_some()
                || !carrier["extensions"]["KHR_lights_punctual"].is_null()
            {
                return Ok(false);
            }
            current = parent_of[at];
        }
    }
    Ok(true)
}
/// Where the model goes, decided before anything moves.
pub struct Placement {
    /// In metres, in the current world frame.
    pub center: [f64; 3],
    /// `POSITION` takes the center, rather than a new parent node.
    pub bake: bool,
    origin: [i64; 3],
    pub source_bounds: BBox,
}
impl Placement {
    pub fn origin_strings(&self) -> [String; 3] {
        self.origin.map(format_fixed_as_decimal)
    }
}
pub fn measure(
    gltf: &Gltf,
    model: &Model,
    parent_of: &[Option<usize>],
    geometry: &Geometry,
    previous: &Previous,
    opts: &Options,
) -> Res<Placement> {
    let bounds = world_bounds(gltf, model, geometry)?;
    let center = if opts.center {
        bounds.center().ok_or("No vertices to center")?
    } else {
        [0; 3]
    };
    let origin = previous.to_source(center)?;
    let mut source_bounds = BBox::new();
    if let Some((min, max)) = bounds.corners() {
        source_bounds.add(previous.to_source(min)?);
        source_bounds.add(previous.to_source(max)?);
    }
    if !opts.quiet {
        eprintln!("Origin: {}", format_triple(origin));
        eprintln!(
            "Bounds: {} (size {})",
            source_bounds.range_str().unwrap_or_default(),
            source_bounds.size_str().unwrap_or_default()
        );
        if !opts.center && !previous.centered {
            eprintln!("Note: coordinates were encoded as float32 without recentering; use --center for georeferenced assets");
        }
    }
    Ok(Placement {
        center: center.map(|c| c as f64 / SCALE as f64),
        bake: opts.center && meshes_can_take_the_center(gltf, model, parent_of, geometry)?,
        origin,
        source_bounds,
    })
}
/// Subtracts the center as local float32 coordinates. The source's float32 values are exact in
/// `f64`, whose rounding here is far below their own.
pub fn bake_center_into_positions(
    gltf: &Gltf,
    map: &mut Patches,
    output: &mut Output,
    ranges: &BTreeMap<usize, PositionRange>,
    center: [f64; 3],
) -> Res<()> {
    for (&new, range) in ranges {
        let a = Accessor::new(gltf, range.old)?;
        let (start, stride) = output.location(new);
        let (mut low, mut high) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for i in 0..a.count {
            let p: [f64; 3] = std::array::from_fn(|c| a.get(i, c) - center[c]);
            map.write_vec3(start + i * stride, p);
            for c in 0..3 {
                low[c] = low[c].min(p[c] as f32);
                high[c] = high[c].max(p[c] as f32);
            }
        }
        output.accessors[new]["min"] = json!(low);
        output.accessors[new]["max"] = json!(high);
    }
    Ok(())
}
/// Moves what baking `POSITION` doesn't: each subtree without meshes, through its own root. That
/// root's parent, if any, is at the world origin, since baking requires it.
pub fn shift_nodes_without_meshes(
    nodes: &mut [Value],
    parent_of: &[Option<usize>],
    center: [f64; 3],
) -> Res<()> {
    let mut bearing = vec![false; nodes.len()];
    for (id, node) in nodes.iter().enumerate() {
        if node.get("mesh").is_some() {
            let mut current = Some(id);
            while let Some(at) = current {
                bearing[at] = true;
                current = parent_of[at];
            }
        }
    }
    for (id, node) in nodes.iter_mut().enumerate() {
        if bearing[id] || parent_of[id].is_some_and(|p| !bearing[p]) {
            continue;
        }
        if node.get("matrix").is_some() {
            let mut m = numbers(node, "matrix", IDENTITY)?;
            for c in 0..3 {
                m[12 + c] -= center[c];
            }
            node["matrix"] = json!(m);
        } else {
            let t = numbers(node, "translation", [0.; 3])?;
            node["translation"] = json!([t[0] - center[0], t[1] - center[1], t[2] - center[2]]);
        }
    }
    Ok(())
}
/// A new parent above the scene roots carries `--up-axis` and any center not baked, rather than
/// the roots themselves: their own transform may be a `matrix`, or be animated. A root in several
/// scenes gets a parent of its own, since a node can have only one. A skinned mesh stays at the
/// root, as the spec recommends: its node's transform is ignored and its joints carry the new
/// parent's. The rotation applies last, as `--up-axis` does in the OBJ path.
pub fn wrap_scene_roots(
    nodes: &mut Vec<Value>,
    scenes: &mut [Value],
    placement: &Placement,
    opts: &Options,
) -> Res<()> {
    let mut transform = Map::new();
    let mut rotation = IDENTITY;
    if opts.z_up {
        let q = std::f64::consts::FRAC_1_SQRT_2;
        transform.insert("rotation".into(), json!([-q, 0., 0., q]));
        rotation = trs([0.; 3], [-q, 0., 0., q], [1.; 3]);
    }
    if opts.center && !placement.bake {
        let moved = apply(&rotation, placement.center.map(|c| -c));
        transform.insert("translation".into(), json!(moved));
    }
    if transform.is_empty() {
        return Ok(());
    }
    let mut uses: BTreeMap<u64, usize> = BTreeMap::new();
    for root in scenes
        .iter()
        .flat_map(|s| s["nodes"].as_array().into_iter().flatten())
    {
        *uses
            .entry(root.as_u64().ok_or("Invalid scene node")?)
            .or_default() += 1;
    }
    let wrapper = |nodes: &mut Vec<Value>, children: Vec<u64>| {
        let mut node = Map::new();
        node.insert("name".into(), json!("objtools"));
        node.insert("children".into(), json!(children));
        node.extend(transform.clone());
        nodes.push(Value::Object(node));
        nodes.len() as u64 - 1
    };
    let mut wrappers: BTreeMap<u64, u64> = BTreeMap::new();
    for scene in scenes.iter_mut() {
        let roots = scene["nodes"].as_array().into_iter().flatten();
        let roots: Vec<u64> = roots.filter_map(Value::as_u64).collect();
        let (mut own, mut shared, mut top) = (Vec::new(), Vec::new(), Vec::new());
        for root in roots {
            let node = &nodes[root as usize];
            if node.get("skin").is_some() && node.get("children").is_none() {
                top.push(root);
            } else if uses[&root] > 1 {
                shared.push(root);
            } else {
                own.push(root);
            }
        }
        if !own.is_empty() {
            top.push(wrapper(nodes, own));
        }
        for root in shared {
            let id = match wrappers.get(&root) {
                Some(id) => *id,
                None => wrapper(nodes, vec![root]),
            };
            wrappers.insert(root, id);
            top.push(id);
        }
        scene["nodes"] = json!(top);
    }
    Ok(())
}
