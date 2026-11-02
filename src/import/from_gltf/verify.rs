//! Checks on the input. Nothing here changes anything: each function either passes or stops the
//! import before a file is written, except where it says it only warns.
use super::{fix::Materials, matrix::instancing, place::Previous, Model};
use crate::{
    import::gltf::MAX_VERTICES,
    shared::{
        gltf::{element, extensions, index, Accessor, Gltf},
        Res,
    },
};
use serde_json::Value;
use std::collections::BTreeSet;
fn primitives(meshes: &[Value]) -> impl Iterator<Item = &Value> {
    meshes
        .iter()
        .flat_map(|m| m["primitives"].as_array().into_iter().flatten())
}
/// An optional Draco extension can only be skipped if the accessors also hold the data.
pub fn compressed_primitives_have_data(root: &Value) -> Res<()> {
    let meshes = root["meshes"].as_array().map_or(&[][..], Vec::as_slice);
    for primitive in primitives(meshes) {
        extensions::check_primitive(root, primitive)?;
    }
    Ok(())
}
/// Applying the rotation again would turn the model on its side.
pub fn up_axis_applies_once(previous: &Previous, z_up: bool) -> Res<()> {
    if previous.z_up && z_up {
        return Err("--up-axis z: the input was already imported with up axis z".into());
    }
    Ok(())
}
pub fn primitive_modes_are_known(meshes: &[Value]) -> Res<()> {
    for primitive in primitives(meshes) {
        let mode = primitive["mode"].as_u64().unwrap_or(4);
        if mode > 6 {
            return Err(format!("Unknown primitive mode {mode}").into());
        }
    }
    Ok(())
}
/// Each node has at most one parent and there is no cycle, as the spec requires. Returns each
/// node's parent, which later steps rely on being a tree.
pub fn hierarchy_is_a_tree(nodes: &[Value]) -> Res<Vec<Option<usize>>> {
    let mut parents = vec![None; nodes.len()];
    for (id, node) in nodes.iter().enumerate() {
        for child in node["children"].as_array().into_iter().flatten() {
            let child = index(child, "child node")?;
            let slot = parents.get_mut(child).ok_or("Node child does not exist")?;
            if slot.replace(id).is_some() {
                return Err(format!("Node {child} has more than one parent").into());
            }
        }
    }
    for start in 0..nodes.len() {
        let (mut node, mut steps) = (start, 0);
        while let Some(parent) = parents[node] {
            node = parent;
            steps += 1;
            if steps > nodes.len() {
                return Err("The node hierarchy has a cycle".into());
            }
        }
    }
    Ok(parents)
}
/// The references later steps follow without checking: from nodes to meshes, from primitives to
/// materials and from scenes to nodes.
pub fn references_resolve(model: &Model) -> Res<()> {
    for node in &model.nodes {
        if let Some(mesh) = node.get("mesh") {
            model
                .meshes
                .get(index(mesh, "mesh")?)
                .ok_or("Node references a missing mesh")?;
        }
    }
    for primitive in primitives(&model.meshes) {
        if let Some(material) = primitive.get("material") {
            model
                .materials
                .get(index(material, "material")?)
                .ok_or("Primitive references a missing material")?;
        }
    }
    for root in scene_roots(&model.scenes) {
        model
            .nodes
            .get(index(root, "scene node")?)
            .ok_or("Scene references a missing node")?;
    }
    Ok(())
}
fn scene_roots(scenes: &[Value]) -> impl Iterator<Item = &Value> {
    scenes
        .iter()
        .flat_map(|s| s["nodes"].as_array().into_iter().flatten())
}
pub fn scene_roots_have_no_parent(scenes: &[Value], parent_of: &[Option<usize>]) -> Res<()> {
    for root in scene_roots(scenes) {
        let root = index(root, "scene node")?;
        if parent_of[root].is_some() {
            return Err(format!("Scene root {root} is another node's child").into());
        }
    }
    Ok(())
}
/// `POSITION` is a VEC3 with data, every attribute has its count, indices are unsigned integer
/// scalars, and a non-indexed primitive can be given indices.
pub fn primitives_are_consistent(gltf: &Gltf, meshes: &[Value]) -> Res<()> {
    for primitive in primitives(meshes) {
        let position = index(&primitive["attributes"]["POSITION"], "POSITION accessor")?;
        let positions = Accessor::new(gltf, position)?;
        if positions.components != 3 || positions.count == 0 {
            return Err(format!("POSITION accessor {position} must be VEC3 with data").into());
        }
        for (name, id) in primitive["attributes"].as_object().into_iter().flatten() {
            let id = index(id, "attribute accessor")?;
            let count = &element(&gltf.root, "accessors", id)?["count"];
            if count.as_u64() != Some(positions.count as u64) {
                return Err(format!("Attribute {name} has a different count from POSITION").into());
            }
        }
        match primitive.get("indices") {
            None if positions.count as u64 > MAX_VERTICES => {
                return Err(
                    format!("Non-indexed primitive exceeds {MAX_VERTICES} vertices").into(),
                );
            }
            None => {}
            Some(id) => {
                let a = Accessor::new(gltf, index(id, "indices")?)?;
                if a.kind == 5126 || a.components != 1 || a.normalized {
                    return Err("Indices must be unsigned integer scalars".into());
                }
            }
        }
    }
    Ok(())
}
/// Every index names a vertex of its primitive, so later steps can follow them unchecked.
pub fn indices_are_in_range(gltf: &Gltf, meshes: &[Value]) -> Res<()> {
    let mut checked = BTreeSet::new();
    for primitive in primitives(meshes) {
        let Some(indices) = primitive.get("indices") else {
            continue;
        };
        let indices = index(indices, "indices")?;
        let position = index(&primitive["attributes"]["POSITION"], "POSITION accessor")?;
        let vertices = element(&gltf.root, "accessors", position)?["count"]
            .as_u64()
            .unwrap_or(0);
        if !checked.insert((indices, vertices)) {
            continue;
        }
        let a = Accessor::new(gltf, indices)?;
        for k in 0..a.count {
            let i = a.get(k, 0) as u64;
            if i >= vertices {
                return Err(format!("Index {i} outside {vertices} vertices").into());
            }
        }
    }
    Ok(())
}
pub fn instancing_counts_agree(gltf: &Gltf, nodes: &[Value]) -> Res<()> {
    for node in nodes {
        if let Some(attributes) = instancing(gltf, node)? {
            let mut counts = attributes.iter().flatten().map(|a| a.count);
            let count = counts.next();
            if counts.any(|other| Some(other) != count) {
                return Err("Instancing attributes have different counts".into());
            }
        }
    }
    Ok(())
}
/// No float the output will carry is NaN or infinite, as in the OBJ path. Bounds, normals and
/// faces are computed from these values afterwards.
pub fn accessors_are_finite(gltf: &Gltf, model: &Model) -> Res<()> {
    let mut used = BTreeSet::new();
    let mut add = |map: &Value| -> Res<()> {
        for (_, id) in map.as_object().into_iter().flatten() {
            used.insert(index(id, "accessor")?);
        }
        Ok(())
    };
    for primitive in primitives(&model.meshes) {
        add(&primitive["attributes"])?;
        for target in primitive["targets"].as_array().into_iter().flatten() {
            add(target)?;
        }
    }
    for node in &model.nodes {
        add(&node["extensions"]["EXT_mesh_gpu_instancing"]["attributes"])?;
    }
    for skin in &model.skins {
        if let Some(id) = skin.get("inverseBindMatrices") {
            used.insert(index(id, "inverseBindMatrices")?);
        }
    }
    for animation in &model.animations {
        for sampler in animation["samplers"].as_array().into_iter().flatten() {
            used.insert(index(&sampler["input"], "animation sampler")?);
            used.insert(index(&sampler["output"], "animation sampler")?);
        }
    }
    for id in used {
        let a = Accessor::new(gltf, id)?;
        if a.kind != 5126 {
            continue;
        }
        for i in 0..a.count {
            for c in 0..a.components {
                if !a.get(i, c).is_finite() {
                    return Err(format!("Accessor {id} has a non-finite value").into());
                }
            }
        }
    }
    Ok(())
}
/// Only warns: a blended material with no opacity is valid, but rarely what anyone meant.
pub fn warn_fully_transparent_materials(materials: &Materials) {
    let sources: BTreeSet<usize> = materials.ids.keys().map(|(source, _)| *source).collect();
    for source in sources {
        let material = &materials.source[source];
        let alpha = &material["pbrMetallicRoughness"]["baseColorFactor"][3];
        if material["alphaMode"] == "BLEND" && alpha == 0. {
            let name = material["name"].as_str().unwrap_or("material");
            eprintln!("Warning: material {name} is fully transparent and will be invisible");
        }
    }
}
