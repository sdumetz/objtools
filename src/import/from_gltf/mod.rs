//! glTF and GLB input. The model is rewritten rather than rebuilt: buffer views are copied whole,
//! so interleaving, sharing and attribute encodings survive, and everything else in the JSON is
//! kept unless a rule below changes it.
//!
//! `verify` only reads, and all of it runs before anything is written. `fix` and `omit` are the
//! only places that change the model, each naming what it establishes, and `place` applies
//! `--center` and `--up-axis`. [`convert`] runs them in the order of this contract.
//!
//! # What the output guarantees
//!
//! Layout
//! - One `.gltf`, one external `.bin` and image files: nothing embedded, whatever the input was.
//!
//! Geometry
//! - Every primitive is an indexed triangle list, with no zero-area triangle.
//! - Indices are `u16`, or `u32` above 65,535 vertices, and all within their vertex count.
//! - A primitive's attributes all have the same count, and no float attribute is non-finite.
//! - `POSITION` bounds are computed from the data.
//! - Float `NORMAL`s used by a triangle have unit length. Quantized ones are left as they are.
//! - Attributes keep the encoding, interleaving and sharing they had, and vertices stay in place.
//!
//! Materials and images
//! - A primitive has every UV set its material samples.
//! - Every material, texture and image in the file is used.
//! - An image is JPG or PNG, or the format of the extension selecting it, with a valid header.
//! - A `mimeType` says what the image really is.
//!
//! Extensions
//! - Only supported extensions appear, and `extensionsUsed` lists exactly those in use.
//! - `extensionsRequired` keeps what the input required, and adds `KHR_texture_transform`.
//!
//! Hierarchy and georeferencing
//! - Nodes keep their indices and the hierarchy is a strict tree. New parents are only appended.
//! - `extras.objtools` records the origin and bounds in the source frame, adding to any earlier.
//!
//! # What it doesn't do
//!
//! What the OBJ path does because of how OBJ works doesn't apply to glTF: V already runs down the
//! image, missing normals mean flat shading, `COLOR_0` is an intended tint and `alphaMode` is
//! authored. Nothing here changes those.
//!
//! Material variants aren't supported. `KHR_materials_variants` is dropped with a warning: each
//! primitive keeps its default material, and what only the variants use goes with them.
mod fix;
mod matrix;
mod omit;
mod payload;
mod place;
mod verify;

use super::{
    gltf::{create, uri, Outputs, COORDINATE_CONVENTION},
    Options,
};
use crate::shared::{disk::parent, gltf::Gltf, Res};
use payload::{Patches, Payload};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{BufWriter, Write},
};
/// The sections of a glTF that get rewritten, taken out of the root so that what remains there is
/// what `Accessor` and `Gltf::image` read.
struct Model {
    meshes: Vec<Value>,
    materials: Vec<Value>,
    nodes: Vec<Value>,
    skins: Vec<Value>,
    animations: Vec<Value>,
    textures: Vec<Value>,
    scenes: Vec<Value>,
    extras: Value,
}
fn take_array(root: &mut Value, key: &str) -> Vec<Value> {
    match root.get_mut(key).map(Value::take) {
        Some(Value::Array(list)) => list,
        _ => Vec::new(),
    }
}
impl Model {
    fn take_from(root: &mut Value) -> Self {
        Self {
            meshes: take_array(root, "meshes"),
            materials: take_array(root, "materials"),
            nodes: take_array(root, "nodes"),
            skins: take_array(root, "skins"),
            animations: take_array(root, "animations"),
            textures: take_array(root, "textures"),
            scenes: take_array(root, "scenes"),
            extras: root.get_mut("extras").map_or(Value::Null, Value::take),
        }
    }
}
fn names(root: &Value, key: &str) -> BTreeSet<String> {
    let list = root[key].as_array().into_iter().flatten();
    list.filter_map(Value::as_str).map(str::to_owned).collect()
}
pub fn convert(opts: &Options, workspace: &std::path::Path) -> Res<()> {
    // Opening verifies that every required extension is supported.
    let mut gltf = Gltf::open(&opts.input, workspace)?;
    let declared = names(&gltf.root, "extensionsUsed");
    let required = names(&gltf.root, "extensionsRequired");
    verify::compressed_primitives_have_data(&gltf.root)?;
    omit::unsupported_extensions(&mut gltf.root, &declared);
    let mut model = Model::take_from(&mut gltf.root);

    // Everything that can make the input unusable fails here, before anything is written.
    let previous = place::Previous::read(&model.extras)?;
    verify::up_axis_applies_once(&previous, opts.z_up)?;
    verify::primitive_modes_are_known(&model.meshes)?;
    omit::primitives_without_triangles(&mut model.meshes);
    if opts.no_color {
        omit::vertex_colors(&mut model.meshes);
    }
    let parent_of = verify::hierarchy_is_a_tree(&model.nodes)?;
    fix::missing_scene(&mut model.scenes, &parent_of);
    verify::references_resolve(&model)?;
    verify::scene_roots_have_no_parent(&model.scenes, &parent_of)?;
    verify::primitives_are_consistent(&gltf, &model.meshes)?;
    verify::indices_are_in_range(&gltf, &model.meshes)?;
    verify::instancing_counts_agree(&gltf, &model.nodes)?;
    verify::accessors_are_finite(&gltf, &model)?;

    // Binary data, written to the workspace.
    let spool = workspace.join("payload.bin");
    let mut payload = Payload::create(&gltf, &spool)?;
    let mut materials = fix::Materials::new(std::mem::take(&mut model.materials));
    let meshes = std::mem::take(&mut model.meshes);
    let geometry = fix::meshes(&gltf, &mut payload, meshes, &mut materials)?;
    verify::warn_fully_transparent_materials(&materials);
    omit::meshes_without_triangles(&mut model.nodes, &geometry.mesh_ids);
    omit::morph_channels_without_mesh(&mut model.animations, &model.nodes);

    // Placement, in the world the nodes describe. This reads instancing attributes from the
    // source, so it comes before they are renumbered.
    let placement = place::measure(&gltf, &model, &parent_of, &geometry, &previous, opts)?;
    payload.other_accessors(&mut model.nodes, &mut model.skins, &mut model.animations)?;
    payload.out.flush()?;
    let mut output = payload.close();
    let mut patches = Patches::open(&spool)?;
    fix::normals(&gltf, &mut patches, &output, &geometry.kept)?;
    if placement.bake {
        place::bake_center_into_positions(
            &gltf,
            &mut patches,
            &mut output,
            &geometry.ranges,
            placement.center,
        )?;
        place::shift_nodes_without_meshes(&mut model.nodes, &parent_of, placement.center)?;
    }
    patches.flush()?;
    drop(patches);
    place::wrap_scene_roots(&mut model.nodes, &mut model.scenes, &placement, opts)?;

    // Images. A file is only published once everything that can fail on the input has passed.
    let convert_all = opts.images.convert == Some(true);
    let mut textures = omit::unused_textures(&mut materials.output, &model.textures, convert_all)?;
    let (images, plans) = fix::plan_images(&gltf, &mut textures, opts, workspace)?;
    let mut outputs = Outputs::new();
    for plan in &plans {
        if let Some(name) = &plan.publish_name {
            let mut dst = create(&parent(&opts.output).join(name), &mut outputs)?;
            std::io::copy(&mut File::open(&plan.content)?, &mut dst)?;
        }
    }
    let bin_path = opts.output.with_extension("bin");
    let mut dst = BufWriter::new(create(&bin_path, &mut outputs)?);
    std::io::copy(&mut File::open(&spool)?, &mut dst)?;
    dst.flush()?;
    drop(dst);

    // The document.
    let mut extras = match model.extras {
        Value::Object(map) => Value::Object(map),
        Value::Null => json!({}),
        other => json!({"source": other}),
    };
    extras["objtools"] = json!({
        "origin": placement.origin_strings(),
        "sourceBounds": placement.source_bounds.to_record(),
        "centered": opts.center || previous.centered,
        "sourceUpAxis": if opts.z_up || previous.z_up {"z"} else {"y"},
        "coordinateConvention": COORDINATE_CONVENTION,
    });
    let mesh_count = geometry.meshes.len();
    let mut root = std::mem::take(&mut gltf.root);
    root["asset"]["generator"] = json!(concat!("objtools ", env!("CARGO_PKG_VERSION")));
    if root.get("scene").is_none() {
        root["scene"] = json!(0);
    }
    root["extras"] = extras;
    let bin_name = bin_path.file_name().unwrap().to_string_lossy();
    root["buffers"] = json!([{"uri": uri(&bin_name), "byteLength": output.length}]);
    root["bufferViews"] = json!(output.views);
    root["accessors"] = json!(output.accessors);
    root["meshes"] = json!(geometry.meshes);
    root["nodes"] = json!(model.nodes);
    root["scenes"] = json!(model.scenes);
    root["materials"] = json!(materials.output);
    root["textures"] = json!(textures);
    root["images"] = json!(images);
    root["skins"] = json!(model.skins);
    root["animations"] = json!(model.animations);
    let map = root.as_object_mut().unwrap();
    map.retain(|_, value| value.as_array().is_none_or(|list| !list.is_empty()));
    if textures.is_empty() {
        map.remove("samplers");
    }
    fix::extension_lists(&mut root, &declared, &required);
    let mut dst = BufWriter::new(create(&opts.output, &mut outputs)?);
    serde_json::to_writer_pretty(&mut dst, &root)?;
    dst.flush()?;
    drop(dst);
    outputs.committed = true;
    if !opts.quiet {
        eprintln!(
            "Wrote {} ({} faces, {mesh_count} meshes, {} bytes of geometry)",
            opts.output.display(),
            geometry.faces,
            output.length
        );
    }
    Ok(())
}
