//! What the output doesn't carry. Each function removes one kind of thing, says so when it isn't
//! obvious from the input, and leaves no reference to it behind.
use super::fix::texture_slots;
use crate::shared::{
    diagnostics::Tally,
    gltf::{extensions, index},
    Res,
};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
fn strip_extensions(value: &mut Value, dropped: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key == "extras" {
                    continue;
                }
                if key == "extensions" {
                    if let Some(list) = child.as_object_mut() {
                        list.retain(|name, _| {
                            let keep = extensions::is_supported(name);
                            if !keep {
                                dropped.insert(name.clone());
                            }
                            keep
                        });
                    }
                }
                strip_extensions(child, dropped);
            }
            let empty = map.get("extensions").and_then(Value::as_object);
            if empty.is_some_and(Map::is_empty) {
                map.remove("extensions");
            }
        }
        Value::Array(list) => list.iter_mut().for_each(|v| strip_extensions(v, dropped)),
        _ => {}
    }
}
/// Every extension nothing here can carry, wherever it appears. What survives only refers to
/// textures and accessors, which are renumbered by name, so this leaves no dangling reference.
/// `extras` belongs to the application and is left alone. Opening the file already warned about
/// the unknown extensions it declares.
pub fn unsupported_extensions(root: &mut Value, declared: &BTreeSet<String>) {
    let mut dropped = BTreeSet::new();
    strip_extensions(root, &mut dropped);
    dropped.retain(|name| !declared.contains(name) || extensions::is_compression(name));
    let mut tally = Tally::new("extensions dropped from the output");
    for name in &dropped {
        tally.add(name);
    }
}
/// Points, lines and primitives without `POSITION`, as OBJ's unsupported records are.
pub fn primitives_without_triangles(meshes: &mut [Value]) {
    let (mut lines, mut no_position) = (0u64, 0u64);
    for mesh in meshes {
        let Some(list) = mesh["primitives"].as_array_mut() else {
            continue;
        };
        list.retain(|primitive| {
            if primitive["mode"].as_u64().is_some_and(|mode| mode < 4) {
                lines += 1;
                false
            } else if primitive["attributes"].get("POSITION").is_none() {
                no_position += 1;
                false
            } else {
                true
            }
        });
    }
    if lines > 0 {
        crate::say!("Warning: omitted {lines} primitives of points or lines");
    }
    if no_position > 0 {
        crate::say!("Warning: omitted {no_position} primitives without POSITION");
    }
}
/// `--no-color`.
pub fn vertex_colors(meshes: &mut [Value]) {
    for mesh in meshes {
        for primitive in mesh["primitives"].as_array_mut().into_iter().flatten() {
            if let Some(map) = primitive["attributes"].as_object_mut() {
                map.retain(|name, _| !name.starts_with("COLOR_"));
            }
        }
    }
}
/// Points nodes at the renumbered meshes. A node whose mesh lost every primitive keeps its index,
/// so skins and animations stay valid, without the mesh or anything that needs one.
pub fn meshes_without_triangles(nodes: &mut [Value], mesh_ids: &[Option<usize>]) {
    for node in nodes {
        let Some(mesh) = node.get("mesh").and_then(Value::as_u64) else {
            continue;
        };
        match mesh_ids[mesh as usize] {
            Some(id) => node["mesh"] = json!(id),
            None => {
                let map = node.as_object_mut().unwrap();
                for key in ["mesh", "skin", "weights"] {
                    map.remove(key);
                }
                if let Some(list) = map.get_mut("extensions").and_then(Value::as_object_mut) {
                    list.remove("EXT_mesh_gpu_instancing");
                    if list.is_empty() {
                        map.remove("extensions");
                    }
                }
            }
        }
    }
}
/// A channel animating morph weights is invalid on a node that lost its mesh.
pub fn morph_channels_without_mesh(animations: &mut [Value], nodes: &[Value]) {
    for animation in animations {
        if let Some(channels) = animation["channels"].as_array_mut() {
            channels.retain(|channel| {
                let target = &channel["target"];
                let node = target["node"].as_u64().and_then(|n| nodes.get(n as usize));
                target["path"] != "weights" || node.is_none_or(|n| n.get("mesh").is_some())
            });
        }
    }
}
/// Keeps only the textures the remaining materials use, renumbered. `--convert-images true`
/// replaces an extension's image, whose fallback then stops being used too.
pub fn unused_textures(
    materials: &mut [Value],
    textures: &[Value],
    convert_all: bool,
) -> Res<Vec<Value>> {
    let mut ids = BTreeMap::new();
    let mut used = Vec::new();
    for material in materials {
        let mut failure = None;
        texture_slots(material, &mut |slot| {
            let mut renumber = || -> Res<usize> {
                let old = index(&slot["index"], "texture")?;
                if let Some(id) = ids.get(&old) {
                    return Ok(*id);
                }
                let texture = textures
                    .get(old)
                    .ok_or("Material references a missing texture")?;
                used.push(texture.clone());
                ids.insert(old, used.len() - 1);
                Ok(used.len() - 1)
            };
            match renumber() {
                Ok(id) => slot["index"] = json!(id),
                Err(e) => failure = Some(e),
            }
            true
        });
        if let Some(e) = failure {
            return Err(e);
        }
    }
    if convert_all {
        for texture in &mut used {
            let image = extensions::texture_source(texture)?;
            let map = texture.as_object_mut().unwrap();
            if let Some(list) = map.get_mut("extensions").and_then(Value::as_object_mut) {
                for name in extensions::IMAGE_EXTENSIONS {
                    list.remove(name);
                }
                if list.is_empty() {
                    map.remove("extensions");
                }
            }
            map.insert("source".into(), json!(image));
        }
    }
    Ok(used)
}
