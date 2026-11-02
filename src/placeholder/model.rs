//! The triangles a glTF written by `objtools import` shows in its default scene, read in place from
//! its mapped buffers, in the coordinates of one frame.
use crate::shared::{
    gltf::{
        element, index,
        matrix::{
            apply, instance_count, instance_matrix, instancing, invert, local_matrix, multiply,
            Instancing, Matrix, IDENTITY,
        },
        Accessor, Gltf,
    },
    Res,
};
use serde_json::{Map, Value};
use std::{
    collections::{btree_map::Entry, BTreeMap, BTreeSet},
    ops::Range,
};
/// An accessor's elements, read directly: this is the hot loop's source.
struct Elements<'a> {
    data: &'a [u8],
    stride: usize,
    count: usize,
}
impl<'a> Elements<'a> {
    fn new(accessor: &Accessor<'a>, what: &str) -> Res<Self> {
        let (data, stride) = accessor
            .dense()
            .ok_or_else(|| format!("Sparse {what} accessors are not supported"))?;
        let count = accessor.count;
        Ok(Self {
            data,
            stride,
            count,
        })
    }
}
struct Primitive<'a> {
    positions: Elements<'a>,
    indices: Elements<'a>,
    /// 32-bit rather than 16-bit indices.
    wide: bool,
    triangles: u64,
    min: [f64; 3],
    max: [f64; 3],
}
impl Primitive<'_> {
    /// Calls `f` for the given faces, seen through `matrix`. It reads floats and indices directly
    /// rather than through `Accessor::get`, since most faces of a model pass through here.
    fn read(
        &self,
        faces: Range<u64>,
        matrix: Option<&Matrix>,
        f: &mut impl FnMut([[f64; 3]; 3]),
    ) -> Result<(), String> {
        let (data, stride) = (self.indices.data, self.indices.stride);
        let corner = |k: usize| -> Result<[f64; 3], usize> {
            let at = k * stride;
            let i = if self.wide {
                u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize
            } else {
                u16::from_le_bytes(data[at..at + 2].try_into().unwrap()) as usize
            };
            if i >= self.positions.count {
                return Err(i);
            }
            let at = i * self.positions.stride;
            let v = &self.positions.data[at..at + 12];
            let p = [
                f64::from(f32::from_le_bytes(v[0..4].try_into().unwrap())),
                f64::from(f32::from_le_bytes(v[4..8].try_into().unwrap())),
                f64::from(f32::from_le_bytes(v[8..12].try_into().unwrap())),
            ];
            Ok(matrix.map_or(p, |m| apply(m, p)))
        };
        let read = |t: u64| -> Result<[[f64; 3]; 3], usize> {
            let t = t as usize;
            Ok([corner(3 * t)?, corner(3 * t + 1)?, corner(3 * t + 2)?])
        };
        for t in faces {
            f(read(t)
                .map_err(|i| format!("Index {i} outside {} vertices", self.positions.count))?);
        }
        Ok(())
    }
}
/// A mesh node of the scene: which primitives it draws, and where.
struct Placement<'a> {
    primitives: Range<usize>,
    /// From the node's coordinates to the frame's. `None` when they are the same, as they are for
    /// every node of most models, which then pay nothing for this.
    to_frame: Option<Matrix>,
    instancing: Option<Instancing<'a>>,
    instances: u64,
}
impl Placement<'_> {
    fn matrix(&self, instance: u64) -> Option<Matrix> {
        match (&self.to_frame, &self.instancing) {
            (to_frame, None) => *to_frame,
            (None, Some(a)) => Some(instance_matrix(a, instance as usize)),
            (Some(m), Some(a)) => Some(multiply(m, &instance_matrix(a, instance as usize))),
        }
    }
}
pub struct Model<'a> {
    primitives: Vec<Primitive<'a>>,
    placements: Vec<Placement<'a>>,
    /// The transforms of the nodes leading to the frame, from the scene's root down, as the source
    /// wrote them. Copying them rather than a composed matrix keeps their digits, and stays valid
    /// glTF when a parent's non-uniform scale shears its child, which no `matrix` may express.
    pub frame: Vec<Value>,
    pub min: [f64; 3],
    pub max: [f64; 3],
    /// Faces the scene draws, so an instanced mesh counts once per instance.
    pub faces: u64,
}
fn vector(value: &Value) -> Res<[f64; 3]> {
    let n = |i: usize| value.get(i).and_then(Value::as_f64);
    match (n(0), n(1), n(2)) {
        (Some(x), Some(y), Some(z)) => Ok([x, y, z]),
        _ => Err("POSITION accessor has no min/max".into()),
    }
}
fn read_primitive<'a>(gltf: &'a Gltf, primitive: &Value) -> Res<Primitive<'a>> {
    if primitive["mode"].as_u64().unwrap_or(4) != 4 {
        return Err("Only triangle primitives are supported".into());
    }
    let position = index(&primitive["attributes"]["POSITION"], "POSITION accessor")?;
    let positions = Accessor::new(gltf, position)?;
    if positions.kind != 5126 || positions.components != 3 {
        return Err("POSITION must be float VEC3".into());
    }
    let indices = Accessor::new(gltf, index(&primitive["indices"], "indices")?)?;
    if !matches!(indices.kind, 5123 | 5125) || indices.components != 1 {
        return Err("Indices must be 16 or 32-bit unsigned scalars".into());
    }
    let accessor = element(&gltf.root, "accessors", position)?;
    Ok(Primitive {
        wide: indices.kind == 5125,
        positions: Elements::new(&positions, "POSITION")?,
        indices: Elements::new(&indices, "index")?,
        triangles: indices.count as u64 / 3,
        min: vector(&accessor["min"])?,
        max: vector(&accessor["max"])?,
    })
}
/// The frame the grid is built in: the first mesh node's own, so that a model whose nodes all share
/// one transform is voxelized in its stored coordinates and placed by that transform alone.
struct Frame {
    world: Matrix,
    inverse: Matrix,
    /// `None` for a skinned node, whose position doesn't depend on any node.
    node: Option<usize>,
}
/// A node's transform properties, or `None` if it has none.
fn transform(node: &Value) -> Option<Value> {
    let kept: Map<String, Value> = ["matrix", "translation", "rotation", "scale"]
        .into_iter()
        .filter_map(|key| Some((key.to_owned(), node.get(key)?.clone())))
        .collect();
    (!kept.is_empty()).then_some(Value::Object(kept))
}
impl<'a> Model<'a> {
    /// Only `objtools import` output is accepted, because `import` has checked its references,
    /// node tree and accessors, and turned its primitives into indexed triangles. What this can't
    /// read, such as quantized positions, is an error here.
    pub fn read(gltf: &'a Gltf) -> Res<Self> {
        let root = &gltf.root;
        if !root["extras"]["objtools"].is_object() {
            return Err(
                "not written by objtools (no extras.objtools); run `objtools import` on its source first"
                    .into(),
            );
        }
        let nodes = &root["nodes"];
        // Like three.js, a file that names no default scene shows its first one.
        let scene = usize::try_from(root["scene"].as_u64().unwrap_or(0))?;
        let scene = element(root, "scenes", scene)?;
        let mut primitives = Vec::new();
        let mut of_mesh = BTreeMap::new();
        let mut placed = Vec::new();
        let mut frame = None;
        let mut parents = BTreeMap::new();
        let mut visited = BTreeSet::new();
        // Depth first in file order, so that which node gives the frame doesn't depend on the walk.
        let mut stack = Vec::new();
        for id in scene["nodes"].as_array().into_iter().flatten().rev() {
            let id = index(id, "scene node")?;
            stack.push((id, local_matrix(element(root, "nodes", id)?)?));
        }
        while let Some((id, world)) = stack.pop() {
            if !visited.insert(id) {
                continue;
            }
            let node = &nodes[id];
            if let Some(mesh) = node.get("mesh") {
                let mesh = index(mesh, "mesh")?;
                if let Entry::Vacant(slot) = of_mesh.entry(mesh) {
                    let first = primitives.len();
                    let list = &element(root, "meshes", mesh)?["primitives"];
                    for primitive in list.as_array().into_iter().flatten() {
                        primitives.push(read_primitive(gltf, primitive)?);
                    }
                    slot.insert(first..primitives.len());
                }
                // The spec ignores a skinned mesh's node, so it counts as stored: its bind pose,
                // not any animated one. `import` does the same when it centers a model.
                let skinned = node.get("skin").is_some();
                let base = if skinned { IDENTITY } else { world };
                if frame.is_none() {
                    frame = invert(&base).map(|inverse| Frame {
                        world: base,
                        inverse,
                        node: (!skinned).then_some(id),
                    });
                }
                let instancing = instancing(gltf, node)?;
                let instances = instancing.as_ref().map_or(1, instance_count) as u64;
                placed.push((of_mesh[&mesh].clone(), base, instancing, instances));
            }
            let children = node["children"].as_array().into_iter().flatten();
            for child in children.rev() {
                let child = index(child, "child node")?;
                parents.insert(child, id);
                let local = local_matrix(element(root, "nodes", child)?)?;
                stack.push((child, multiply(&world, &local)));
            }
        }
        if placed.is_empty() {
            return Err("The default scene shows no mesh".into());
        }
        // A model flattened by all its transforms has no frame of its own to share, and is built
        // in world coordinates.
        let frame = frame.unwrap_or(Frame {
            world: IDENTITY,
            inverse: IDENTITY,
            node: None,
        });
        let mut model = Self {
            primitives,
            placements: Vec::with_capacity(placed.len()),
            frame: Vec::new(),
            min: [f64::INFINITY; 3],
            max: [f64::NEG_INFINITY; 3],
            faces: 0,
        };
        let mut next = frame.node;
        while let Some(id) = next {
            model.frame.extend(transform(&nodes[id]));
            next = parents.get(&id).copied();
        }
        model.frame.reverse();
        for (primitives, world, instancing, instances) in placed {
            let to_frame = (world != frame.world).then(|| multiply(&frame.inverse, &world));
            model.placements.push(Placement {
                primitives,
                to_frame,
                instancing,
                instances,
            });
        }
        model.measure();
        Ok(model)
    }
    /// Bounds are each primitive's box seen through its placement. Under a rotation that can be
    /// slightly larger than the geometry, which voxelizing again on a fitted grid makes up for:
    /// reading every vertex once per instance wouldn't scale.
    fn measure(&mut self) {
        let (mut min, mut max) = (self.min, self.max);
        let mut faces = 0;
        for placement in &self.placements {
            for p in &self.primitives[placement.primitives.clone()] {
                faces += p.triangles * placement.instances;
                if p.triangles == 0 {
                    continue;
                }
                for instance in 0..placement.instances {
                    let matrix = placement.matrix(instance);
                    for corner in 0..8 {
                        let v = std::array::from_fn(|k| {
                            if corner >> k & 1 == 0 {
                                p.min[k]
                            } else {
                                p.max[k]
                            }
                        });
                        let v = matrix.as_ref().map_or(v, |m| apply(m, v));
                        for k in 0..3 {
                            min[k] = min[k].min(v[k]);
                            max[k] = max[k].max(v[k]);
                        }
                    }
                }
            }
        }
        (self.min, self.max, self.faces) = (min, max, faces);
    }
    /// Calls `f` for faces `start..end` of the whole scene, in order, in the frame's coordinates.
    pub fn triangles(
        &self,
        start: u64,
        end: u64,
        mut f: impl FnMut([[f64; 3]; 3]),
    ) -> Result<(), String> {
        let mut base = 0;
        for placement in &self.placements {
            for p in &self.primitives[placement.primitives.clone()] {
                let total = p.triangles * placement.instances;
                let (first, last) = (start.max(base), end.min(base + total));
                if first < last {
                    let (first, last) = (first - base, last - base);
                    for instance in first / p.triangles..=(last - 1) / p.triangles {
                        let at = instance * p.triangles;
                        let faces = first.saturating_sub(at)..(last - at).min(p.triangles);
                        p.read(faces, placement.matrix(instance).as_ref(), &mut f)?;
                    }
                }
                base += total;
            }
        }
        Ok(())
    }
}
