//! Which glTF extensions can be read, and the texture image they select.
use crate::shared::Res;
use serde_json::Value;
/// Read or carried through by both commands. `inspect` applies the texture extensions and needs
/// nothing from the others to compute its statistics; `import` keeps them all in its output.
const SUPPORTED: &[&str] = &[
    "EXT_materials_bump",
    "EXT_mesh_gpu_instancing",
    "EXT_texture_avif",
    "EXT_texture_webp",
    "KHR_lights_punctual",
    "KHR_materials_anisotropy",
    "KHR_materials_clearcoat",
    "KHR_materials_diffuse_roughness",
    "KHR_materials_dispersion",
    "KHR_materials_emissive_strength",
    "KHR_materials_ior",
    "KHR_materials_iridescence",
    "KHR_materials_sheen",
    "KHR_materials_specular",
    "KHR_materials_transmission",
    "KHR_materials_unlit",
    "KHR_materials_volume",
    "KHR_mesh_quantization",
    "KHR_texture_basisu",
    "KHR_texture_transform",
];
/// Not decoded, but valid to skip when optional: the file then carries uncompressed data too.
const COMPRESSION: &[&str] = &[
    "EXT_meshopt_compression",
    "KHR_draco_mesh_compression",
    "KHR_meshopt_compression",
];
/// Not applied: a primitive only ever uses its default material, so the warning says what is lost
/// instead of only naming the extension.
const VARIANTS: &str = "KHR_materials_variants";
/// The texture extensions that select an image, in the order `texture_source` prefers them.
pub const IMAGE_EXTENSIONS: [&str; 3] =
    ["KHR_texture_basisu", "EXT_texture_webp", "EXT_texture_avif"];
pub fn is_supported(name: &str) -> bool {
    SUPPORTED.contains(&name)
}
pub fn is_compression(name: &str) -> bool {
    COMPRESSION.contains(&name)
}
/// A required extension can't be ignored without misreading the file, as the spec says. An
/// optional one can, so it only warns.
pub fn check(root: &Value) -> Res<()> {
    let names = |key: &str| {
        let list = root[key].as_array().into_iter().flatten();
        list.filter_map(Value::as_str).collect::<Vec<_>>()
    };
    if let Some(name) = names("extensionsRequired")
        .into_iter()
        .find(|name| !SUPPORTED.contains(name))
    {
        return Err(format!("Required extension {name} is not supported").into());
    }
    let mut ignored = names("extensionsUsed");
    ignored.retain(|name| !SUPPORTED.contains(name) && !COMPRESSION.contains(name));
    if ignored.contains(&VARIANTS) {
        eprintln!(
            "Warning: material variants are not supported ({VARIANTS}): only each primitive's default material is used"
        );
        ignored.retain(|name| *name != VARIANTS);
    }
    if !ignored.is_empty() {
        eprintln!(
            "Warning: ignored {} unsupported extensions: {}",
            ignored.len(),
            ignored.join(", ")
        );
    }
    Ok(())
}
/// An optional Draco extension is only skippable if the primitive's accessors also hold the
/// data. The spec reads an accessor without a buffer view as zeros, but here it means the data
/// is only in the compressed stream.
pub fn check_primitive(root: &Value, primitive: &Value) -> Res<()> {
    if primitive["extensions"]
        .get("KHR_draco_mesh_compression")
        .is_none()
    {
        return Ok(());
    }
    let attributes = primitive["attributes"].as_object().into_iter().flatten();
    let accessors = attributes.map(|(_, id)| id).chain(primitive.get("indices"));
    for id in accessors.filter_map(Value::as_u64) {
        let accessor = &root["accessors"][usize::try_from(id)?];
        if accessor.get("bufferView").is_none() {
            return Err(
                "KHR_draco_mesh_compression is not supported and the primitive has no uncompressed data"
                    .into(),
            );
        }
    }
    Ok(())
}
/// The image a renderer shows for a texture. An extension's image takes precedence over the
/// `source` fallback, in the order three.js's `GLTFLoader` registers its plugins.
pub fn texture_source(texture: &Value) -> Res<usize> {
    let extension = IMAGE_EXTENSIONS
        .iter()
        .find_map(|name| texture["extensions"][name]["source"].as_u64());
    let image = extension
        .or_else(|| texture["source"].as_u64())
        .ok_or("Missing or invalid texture source image index")?;
    Ok(usize::try_from(image)?)
}
