//! Writes the placeholder as glTF, in the coordinates of its source. Its checker texture is stored
//! in the buffer, so a placeholder is still one .gltf and one .bin.
use super::{model::Model, surface::Mesh, Options};
use crate::shared::{disk::Outputs, gltf::uri, Res};
use serde_json::{json, Value};
use std::io::{BufWriter, Write};
/// glTF reserves each index type's maximum value for primitive restart.
const SHORT_VERTICES: usize = u16::MAX as usize;
const STRIDE: usize = 32;
const COLORS: [[u8; 3]; 2] = [[0x00, 0x89, 0xbf], [0xd9, 0xd9, 0xd9]];
/// Enough texels that the squares' edges stay sharp when a renderer filters linearly, as Blender
/// does for this sampler. A two-texel image would rely on nearest filtering, and blur without it.
const CHECKER_SIZE: usize = 64;
fn checker_png() -> Res<Vec<u8>> {
    let half = CHECKER_SIZE / 2;
    let mut pixels = Vec::with_capacity(CHECKER_SIZE * CHECKER_SIZE * 3);
    for y in 0..CHECKER_SIZE {
        for x in 0..CHECKER_SIZE {
            pixels.extend_from_slice(&COLORS[usize::from((x < half) != (y < half))]);
        }
    }
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, CHECKER_SIZE as u32, CHECKER_SIZE as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&pixels)?;
    writer.finish()?;
    Ok(png)
}
pub fn write(mesh: &Mesh, source: &Value, model: &Model, opts: &Options) -> Res<()> {
    let short = mesh.positions.len() <= SHORT_VERTICES;
    let vertex_bytes = mesh.positions.len() * STRIDE;
    let index_bytes = mesh.indices.len() * if short { 2 } else { 4 };
    let image = checker_png()?;
    let (mut min, mut max) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for p in &mesh.positions {
        for k in 0..3 {
            min[k] = min[k].min(p[k]);
            max[k] = max[k].max(p[k]);
        }
    }
    let bin_path = opts.output.with_extension("bin");
    let mut outputs = Outputs::default();
    let mut bin = BufWriter::new(outputs.create(&bin_path)?);
    for ((p, n), uv) in mesh.positions.iter().zip(&mesh.normals).zip(&mesh.uvs) {
        for c in p.iter().chain(n).chain(uv) {
            bin.write_all(&c.to_le_bytes())?;
        }
    }
    for i in &mesh.indices {
        if short {
            bin.write_all(&(*i as u16).to_le_bytes())?;
        } else {
            bin.write_all(&i.to_le_bytes())?;
        }
    }
    bin.write_all(&image)?;
    bin.flush()?;
    drop(bin);
    // Positions stay in the frame of the source's first mesh node, so the placeholder needs the
    // transforms leading there to overlay the model.
    let mut nodes = model.frame.clone();
    if nodes.is_empty() {
        nodes.push(json!({}));
    }
    let leaf = nodes.len() - 1;
    for (i, node) in nodes.iter_mut().enumerate().take(leaf) {
        node["children"] = json!([i + 1]);
    }
    nodes[leaf]["name"] = json!("placeholder");
    nodes[leaf]["mesh"] = json!(0);
    let mut extras = source["extras"]["objtools"].clone();
    extras["placeholder"] = json!({"resolution":opts.resolution,"sourceFaces":model.faces});
    let bin_name = bin_path.file_name().unwrap().to_string_lossy();
    let root = json!({
        "asset":{"version":"2.0","generator":concat!("objtools ",env!("CARGO_PKG_VERSION"))},
        "scene":0,"scenes":[{"nodes":[0]}],"nodes":nodes,
        "meshes":[{"name":"placeholder","primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":3},"indices":2,"material":0,"mode":4}]}],
        "materials":[{"name":"placeholder","pbrMetallicRoughness":{"baseColorTexture":{"index":0},"metallicFactor":0.0,"roughnessFactor":1.0}}],
        "textures":[{"source":0,"sampler":0}],
        "images":[{"name":"placeholder-checker","bufferView":2,"mimeType":"image/png"}],
        // Mipmaps let distant squares fade to their average instead of shimmering.
        "samplers":[{"magFilter":9729,"minFilter":9987,"wrapS":10497,"wrapT":10497}],
        "buffers":[{"uri":uri(&bin_name),"byteLength":vertex_bytes + index_bytes + image.len()}],
        "bufferViews":[
            {"buffer":0,"byteLength":vertex_bytes,"byteStride":STRIDE,"target":34962},
            {"buffer":0,"byteOffset":vertex_bytes,"byteLength":index_bytes,"target":34963},
            {"buffer":0,"byteOffset":vertex_bytes + index_bytes,"byteLength":image.len()}
        ],
        "accessors":[
            {"bufferView":0,"componentType":5126,"count":mesh.positions.len(),"type":"VEC3","min":min,"max":max},
            {"bufferView":0,"byteOffset":12,"componentType":5126,"count":mesh.positions.len(),"type":"VEC3"},
            {"bufferView":1,"componentType":if short {5123} else {5125},"count":mesh.indices.len(),"type":"SCALAR"},
            {"bufferView":0,"byteOffset":24,"componentType":5126,"count":mesh.positions.len(),"type":"VEC2"}
        ],
        "extras":{"objtools":extras}
    });
    let mut gltf = BufWriter::new(outputs.create(&opts.output)?);
    serde_json::to_writer_pretty(&mut gltf, &root)?;
    gltf.flush()?;
    drop(gltf);
    outputs.commit();
    if opts.progress {
        eprintln!(
            "Wrote {} ({} faces from {})",
            opts.output.display(),
            mesh.indices.len() / 3,
            model.faces
        );
    }
    Ok(())
}
