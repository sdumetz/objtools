use serde_json::{json, Value};
use std::{
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
            "objtools-inspect-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn file(&self, name: &str, body: &str) {
        fs::write(self.0.join(name), body).unwrap();
    }
    fn run(&self, args: &[&str], input: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_objtools"))
            .args(args)
            .arg(self.0.join(input))
            .output()
            .unwrap()
    }
    fn inspect(&self, input: &str) -> String {
        let out = self.run(&["inspect"], input);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
    fn inspect_json(&self, input: &str) -> Value {
        let out = self.run(&["inspect", "--json"], input);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
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
/// A quad textured with texels x 2..4, y 2..6 of an 8x8 image, then the same quad again in a
/// second object. Import's sampler leaves filtering to the engine default, linear, which also
/// reads the texels within half a texel of that region: columns 1..=4 and rows 1..=6.
fn half_textured(image: &str) -> Fixture {
    let f = Fixture::new();
    image::RgbImage::new(8, 8).save(f.0.join(image)).unwrap();
    f.file("in.mtl", &format!("newmtl paint\nmap_Kd {image}\n"));
    f.file("in.obj", "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt .25 .25\nvt .5 .25\nvt .5 .75\nvt .25 .75\nusemtl paint\no first\nf 1/1 2/2 3/3 4/4\no second\nf 1/1 2/2 3/3 4/4\n");
    f
}
#[test]
fn reports_counts_and_counts_overlapping_texels_once() {
    let f = half_textured("source.png");
    let report = f.inspect("in.obj");
    assert_eq!(
        report,
        "Vertices:           8\nFaces:              4\nDiffuse texels:     64 in 1 texture\nTexture occupancy:  0.375\n\nTexture            Size  Occupancy\nsource.png          8x8  0.375\n"
    );
    // The import workspace and coverage maps are gone; nothing was published.
    assert_eq!(f.entries(), ["in.mtl", "in.obj", "source.png"]);
}
#[test]
fn json_reports_the_same_statistics_unrounded() {
    let f = half_textured("source.png");
    assert_eq!(
        f.inspect_json("in.obj"),
        json!({
            "vertices": 8,
            "faces": 4,
            "diffuse_texels": 64,
            "occupied_texels": 24,
            "texture_occupancy": 0.375,
            "textures": [{
                "name": "source.png",
                "width": 8,
                "height": 8,
                "occupied_texels": 24,
                "occupancy": 0.375,
            }],
        })
    );
    assert_eq!(f.entries(), ["in.mtl", "in.obj", "source.png"]);
}
#[test]
fn json_of_an_untextured_model_has_no_occupancy() {
    let f = Fixture::new();
    f.file("in.obj", "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n");
    let report = f.inspect_json("in.obj");
    assert_eq!(report["faces"], 1);
    assert_eq!(report["texture_occupancy"], Value::Null);
    assert_eq!(report["textures"], json!([]));
}
#[test]
fn json_stdout_stays_parseable_with_warnings_and_progress() {
    // The undefined material warns during the private import.
    let f = Fixture::new();
    f.file(
        "in.obj",
        "v 0 0 0\nv 1 0 0\nv 0 1 0\nusemtl undefined\nf 1 2 3\n",
    );
    let out = f.run(&["inspect", "--json", "--progress"], "in.obj");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Warning: undefined material"));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["vertices"], 3);
}
#[test]
fn obj_report_matches_report_of_imported_gltf() {
    let f = half_textured("source.png");
    let out = f.run(&["import"], "in.obj");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(f.inspect("in.obj"), f.inspect("in.gltf"));
    assert_eq!(f.inspect_json("in.obj"), f.inspect_json("in.gltf"));
}
#[test]
fn vertex_colors_are_imported_as_by_default() {
    let f = Fixture::new();
    f.file(
        "in.obj",
        "v 0 0 0 2 0 0\nv 1 0 0 0 1 0\nv 1 1 0 0 0 1\nf 1 2 3\n",
    );
    let out = f.run(&["inspect"], "in.obj");
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("vertex colors outside [0,1] are clamped"),
        "{stderr}"
    );
    assert!(f.run(&["import"], "in.obj").status.success());
    assert_eq!(f.inspect("in.obj"), f.inspect("in.gltf"));
}
#[test]
fn textures_gltf_cannot_carry_are_measured_without_conversion() {
    let f = half_textured("source.bmp");
    assert!(f
        .inspect("in.obj")
        .contains("source.bmp          8x8  0.375\n"));
    assert_eq!(f.entries(), ["in.mtl", "in.obj", "source.bmp"]);
}
#[test]
fn untextured_models_and_large_counts() {
    let f = Fixture::new();
    let mut obj = String::from("v 0 0 0\nv 1 0 0\nv 0 1 0\n");
    obj.push_str(&"f 1 2 3\n".repeat(1500));
    f.file("in.obj", &obj);
    let report = f.inspect("in.obj");
    assert!(report.contains("Faces:              1.50k\n"), "{report}");
    assert!(report.contains("Diffuse texels:     0 in 0 textures\nTexture occupancy:  -\n"));
    assert!(!report.contains("Size"));
}
#[test]
fn unsupported_inputs_are_errors() {
    let f = Fixture::new();
    f.file("model.stl", "solid x\n");
    let out = f.run(&["inspect"], "model.stl");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Unsupported input format"));
    assert_eq!(f.entries(), ["model.stl"]);
}
#[test]
fn texture_transform_is_applied_as_renderers_sample() {
    // Face 2's material maps its UVs onto face 1's texels: `s·vt + o` with V up, as in MTL.
    // If the transform, or its conversion to glTF's flipped V, were wrong, the two triangles
    // would cover different texels and the union would exceed one triangle's.
    let f = Fixture::new();
    image::RgbImage::new(64, 64)
        .save(f.0.join("tex.png"))
        .unwrap();
    f.file(
        "moved.mtl",
        "newmtl plain\nmap_Kd tex.png\nnewmtl moved\nmap_Kd -s 0.5 0.25 1 -o 0.125 0.5 tex.png\n",
    );
    let positions = "v 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\nv 1 0 1\nv 0 1 1\n";
    f.file(
        "moved.obj",
        &format!("mtllib moved.mtl\n{positions}vt .125 .5\nvt .375 .5\nvt .125 .75\nvt 0 0\nvt .5 0\nvt 0 1\nusemtl plain\nf 1/1 2/2 3/3\nusemtl moved\nf 4/4 5/5 6/6\n"),
    );
    f.file("baked.mtl", "newmtl plain\nmap_Kd tex.png\n");
    f.file(
        "baked.obj",
        &format!("mtllib baked.mtl\n{positions}vt .125 .5\nvt .375 .5\nvt .125 .75\nusemtl plain\nf 1/1 2/2 3/3\nf 4/1 5/2 6/3\n"),
    );
    assert_eq!(f.inspect("moved.obj"), f.inspect("baked.obj"));
}
/// `half_textured` imported to `in.gltf`, then edited: the quad on an 8x8 `source.png`.
fn imported_gltf(edit: impl FnOnce(&mut Value)) -> Fixture {
    let f = half_textured("source.png");
    assert!(f.run(&["import"], "in.obj").status.success());
    let mut root: Value = serde_json::from_slice(&fs::read(f.0.join("in.gltf")).unwrap()).unwrap();
    edit(&mut root);
    f.file("in.gltf", &root.to_string());
    f
}
/// A KTX2 header: identifier, vkFormat, typeSize, then the size. Nothing past it is read.
fn ktx2(width: u32, height: u32) -> Vec<u8> {
    let mut out = b"\xabKTX 20\xbb\r\n\x1a\n".to_vec();
    for field in [0, 1, width, height] {
        out.extend(field.to_le_bytes());
    }
    out
}
/// The AVIF boxes leading to the image size: ftyp, then meta > iprp > ipco > ispe.
fn avif(width: u32, height: u32) -> Vec<u8> {
    let boxed = |tag: &[u8; 4], body: &[u8]| {
        let mut out = (body.len() as u32 + 8).to_be_bytes().to_vec();
        out.extend(tag);
        out.extend(body);
        out
    };
    let mut ispe = vec![0; 4];
    ispe.extend(width.to_be_bytes());
    ispe.extend(height.to_be_bytes());
    let mut meta = vec![0; 4];
    meta.extend(boxed(b"iprp", &boxed(b"ipco", &boxed(b"ispe", &ispe))));
    let mut out = boxed(b"ftyp", b"avif\0\0\0\0avif");
    out.extend(boxed(b"meta", &meta));
    out
}
/// The two statistics an image's size decides, for the same UVs.
fn texels(report: &Value) -> (&Value, &Value) {
    (&report["diffuse_texels"], &report["textures"][0]["name"])
}
#[test]
fn extension_images_replace_their_fallback() {
    for (extension, name, bytes) in [
        ("KHR_texture_basisu", "extra.ktx2", ktx2(16, 16)),
        ("EXT_texture_avif", "extra.avif", avif(16, 16)),
    ] {
        for fallback in [true, false] {
            let f = imported_gltf(|root| {
                root["images"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"uri": name}));
                let texture = &mut root["textures"][0];
                texture["extensions"] = json!({extension: {"source": 1}});
                if !fallback {
                    texture.as_object_mut().unwrap().remove("source");
                }
                root["extensionsUsed"] = json!([extension]);
            });
            fs::write(f.0.join(name), &bytes).unwrap();
            let report = f.inspect_json("in.gltf");
            assert_eq!(texels(&report), (&json!(256), &json!(name)), "{extension}");
            assert_eq!(report["textures"][0]["occupied_texels"], 60, "{extension}");
        }
    }
}
#[test]
fn webp_extension_image_replaces_its_fallback() {
    let f = imported_gltf(|root| {
        root["images"]
            .as_array_mut()
            .unwrap()
            .push(json!({"uri": "extra.webp"}));
        root["textures"][0]["extensions"] = json!({"EXT_texture_webp": {"source": 1}});
        root["extensionsUsed"] = json!(["EXT_texture_webp"]);
    });
    image::RgbImage::new(16, 16)
        .save(f.0.join("extra.webp"))
        .unwrap();
    let report = f.inspect_json("in.gltf");
    assert_eq!(texels(&report), (&json!(256), &json!("extra.webp")));
}
#[test]
fn texture_extensions_follow_the_order_of_three_js() {
    let f = imported_gltf(|root| {
        let images = root["images"].as_array_mut().unwrap();
        images.push(json!({"uri": "extra.ktx2"}));
        images.push(json!({"uri": "extra.avif"}));
        root["textures"][0]["extensions"] = json!({
            "EXT_texture_avif": {"source": 2},
            "KHR_texture_basisu": {"source": 1},
        });
    });
    fs::write(f.0.join("extra.ktx2"), ktx2(16, 16)).unwrap();
    fs::write(f.0.join("extra.avif"), avif(32, 32)).unwrap();
    assert_eq!(
        f.inspect_json("in.gltf")["textures"][0]["name"],
        "extra.ktx2"
    );
}
#[test]
fn container_sizes_are_read_from_buffer_views() {
    for (mime, bytes) in [("image/ktx2", ktx2(16, 4)), ("image/avif", avif(16, 4))] {
        let mut length = 0;
        let f = imported_gltf(|root| {
            length = root["buffers"][0]["byteLength"].as_u64().unwrap();
            let view = root["bufferViews"].as_array().unwrap().len();
            root["bufferViews"]
                .as_array_mut()
                .unwrap()
                .push(json!({"buffer": 0, "byteOffset": length, "byteLength": bytes.len()}));
            root["buffers"][0]["byteLength"] = json!(length + bytes.len() as u64);
            root["images"] = json!([{"bufferView": view, "mimeType": mime}]);
        });
        let mut bin = fs::read(f.0.join("in.bin")).unwrap();
        assert_eq!(bin.len() as u64, length);
        bin.extend(&bytes);
        fs::write(f.0.join("in.bin"), bin).unwrap();
        let report = f.inspect_json("in.gltf");
        assert_eq!(report["textures"][0]["width"], 16, "{mime}");
        assert_eq!(report["textures"][0]["height"], 4, "{mime}");
    }
}
#[test]
fn a_container_without_a_2d_size_is_an_error() {
    let f = imported_gltf(|root| root["images"] = json!([{"uri": "line.ktx2"}]));
    fs::write(f.0.join("line.ktx2"), ktx2(16, 0)).unwrap();
    let out = f.run(&["inspect"], "in.gltf");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("line.ktx2"));
}
#[test]
fn unsupported_required_extensions_are_errors() {
    for name in [
        "VENDOR_made_up",
        "KHR_draco_mesh_compression",
        "EXT_meshopt_compression",
        "KHR_meshopt_compression",
    ] {
        let f = imported_gltf(|root| {
            root["extensionsUsed"] = json!([name]);
            root["extensionsRequired"] = json!([name]);
        });
        let out = f.run(&["inspect"], "in.gltf");
        assert!(!out.status.success(), "{name}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(name), "{stderr}");
    }
}
#[test]
fn supported_required_extensions_are_accepted() {
    let f = imported_gltf(|root| {
        let names = ["KHR_mesh_quantization", "EXT_mesh_gpu_instancing"];
        root["extensionsUsed"] = json!(names);
        root["extensionsRequired"] = json!(names);
    });
    let out = f.run(&["inspect"], "in.gltf");
    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
#[test]
fn unknown_optional_extensions_warn_once_and_change_nothing() {
    let f = imported_gltf(|root| {
        root["extensionsUsed"] = json!(["VENDOR_one", "KHR_materials_ior", "VENDOR_two"]);
    });
    let out = f.run(&["inspect", "--json"], "in.gltf");
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr,
        "Warning: ignored 2 unsupported extensions: VENDOR_one, VENDOR_two\n"
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["occupied_texels"], 24);
}
#[test]
fn optional_draco_uses_uncompressed_data_or_is_an_error() {
    let draco = |root: &mut Value| {
        root["extensionsUsed"] = json!(["KHR_draco_mesh_compression"]);
        for mesh in root["meshes"].as_array_mut().unwrap() {
            for primitive in mesh["primitives"].as_array_mut().unwrap() {
                primitive["extensions"] = json!({"KHR_draco_mesh_compression": {
                    "bufferView": 0,
                    "attributes": {"POSITION": 0, "TEXCOORD_0": 1},
                }});
            }
        }
    };
    let f = imported_gltf(draco);
    let out = f.run(&["inspect", "--json"], "in.gltf");
    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["occupied_texels"], 24);

    let f = imported_gltf(|root| {
        draco(root);
        for accessor in root["accessors"].as_array_mut().unwrap() {
            accessor.as_object_mut().unwrap().remove("bufferView");
        }
    });
    let out = f.run(&["inspect"], "in.gltf");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("KHR_draco_mesh_compression"), "{stderr}");
}
fn read_gltf(f: &Fixture) -> Value {
    serde_json::from_slice(&fs::read(f.0.join("in.gltf")).unwrap()).unwrap()
}
/// A GLB holding `root` and an optional BIN chunk, with the padding the container requires.
fn glb(root: &Value, bin: Option<&[u8]>) -> Vec<u8> {
    let mut json = root.to_string().into_bytes();
    json.resize(json.len().next_multiple_of(4), b' ');
    let mut chunks = (json.len() as u32).to_le_bytes().to_vec();
    chunks.extend(b"JSON");
    chunks.extend(&json);
    if let Some(bin) = bin {
        let mut bin = bin.to_vec();
        bin.resize(bin.len().next_multiple_of(4), 0);
        chunks.extend((bin.len() as u32).to_le_bytes());
        chunks.extend(b"BIN\0");
        chunks.extend(&bin);
    }
    let mut out = b"glTF".to_vec();
    out.extend(2u32.to_le_bytes());
    out.extend((chunks.len() as u32 + 12).to_le_bytes());
    out.extend(chunks);
    out
}
/// `in.gltf` and its geometry as a GLB, with the external image left as it is.
fn packed() -> (Fixture, Value, Vec<u8>) {
    let f = imported_gltf(|root| {
        root["buffers"][0].as_object_mut().unwrap().remove("uri");
    });
    let root = read_gltf(&f);
    let bin = fs::read(f.0.join("in.bin")).unwrap();
    (f, root, bin)
}
fn base64(bytes: &[u8]) -> String {
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
}
#[test]
fn glb_reports_the_same_statistics_as_gltf() {
    let expected = half_textured("source.png");
    assert!(expected.run(&["import"], "in.obj").status.success());
    let (f, root, bin) = packed();
    fs::remove_file(f.0.join("in.bin")).unwrap();
    fs::write(f.0.join("in.glb"), glb(&root, Some(&bin))).unwrap();
    assert_eq!(f.inspect_json("in.glb"), expected.inspect_json("in.gltf"));
    assert!(!f.entries().iter().any(|e| e.starts_with("objtools-")));
}
#[test]
fn glb_images_are_read_from_the_bin_chunk() {
    let (f, mut root, mut bin) = packed();
    let png = fs::read(f.0.join("source.png")).unwrap();
    fs::remove_file(f.0.join("source.png")).unwrap();
    let view = root["bufferViews"].as_array().unwrap().len();
    root["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(json!({"buffer": 0, "byteOffset": bin.len(), "byteLength": png.len()}));
    bin.extend(&png);
    root["buffers"][0]["byteLength"] = json!(bin.len());
    root["images"] = json!([{"bufferView": view, "mimeType": "image/png", "name": "paint"}]);
    fs::write(f.0.join("in.glb"), glb(&root, Some(&bin))).unwrap();
    let report = f.inspect_json("in.glb");
    assert_eq!(report["occupied_texels"], 24);
    assert_eq!(report["textures"][0]["name"], "paint");
}
#[test]
fn glb_is_recognized_by_content_and_skips_unknown_chunks() {
    let (f, root, bin) = packed();
    let mut file = glb(&root, Some(&bin));
    let mut unknown = 4u32.to_le_bytes().to_vec();
    unknown.extend(b"XTRA\0\0\0\0");
    file.extend(&unknown);
    let length = file.len() as u32;
    file[8..12].copy_from_slice(&length.to_le_bytes());
    fs::write(f.0.join("packed.gltf"), &file).unwrap();
    assert_eq!(f.inspect_json("packed.gltf")["occupied_texels"], 24);
}
#[test]
fn malformed_glb_files_are_errors() {
    let (f, root, bin) = packed();
    let good = glb(&root, Some(&bin));
    let mut version = good.clone();
    version[4] = 1;
    let mut long_chunk = good.clone();
    long_chunk[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut only_header = good[..12].to_vec();
    only_header[8..12].copy_from_slice(&12u32.to_le_bytes());
    let mut second_buffer = root.clone();
    second_buffer["buffers"]
        .as_array_mut()
        .unwrap()
        .push(json!({"byteLength": 4}));
    let mut short = root.clone();
    short["buffers"][0]["byteLength"] = json!(bin.len() + 64);
    for (file, message) in [
        (good[..4].to_vec(), "Truncated GLB"),
        (good[..good.len() - 1].to_vec(), "Truncated GLB"),
        (version, "Unsupported GLB version 1"),
        (long_chunk, "Truncated GLB"),
        (only_header, "no JSON chunk"),
        (glb(&root, None), "Buffer 0 has no URI"),
        (glb(&second_buffer, Some(&bin)), "Buffer 1 has no URI"),
        (glb(&short, Some(&bin)), "shorter than its byteLength"),
    ] {
        fs::write(f.0.join("bad.glb"), file).unwrap();
        let out = f.run(&["inspect"], "bad.glb");
        assert!(!out.status.success(), "{message}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(message), "{message}: {stderr}");
        assert!(!f.entries().iter().any(|e| e.starts_with("objtools-")));
    }
}
#[test]
fn data_uris_are_decoded_without_external_files() {
    let f = half_textured("source.png");
    assert!(f.run(&["import"], "in.obj").status.success());
    let mut root = read_gltf(&f);
    let bin = base64(&fs::read(f.0.join("in.bin")).unwrap());
    let png = base64(&fs::read(f.0.join("source.png")).unwrap());
    root["buffers"][0]["uri"] = json!(format!("data:application/octet-stream;base64,{bin}"));
    root["images"][0]["uri"] = json!(format!("data:image/png;base64,{png}"));
    for name in ["in.bin", "source.png", "in.gltf", "in.obj", "in.mtl"] {
        fs::remove_file(f.0.join(name)).unwrap();
    }
    f.file("in.gltf", &root.to_string());
    let report = f.inspect_json("in.gltf");
    assert_eq!(report["occupied_texels"], 24);
    assert_eq!(report["textures"][0]["name"], "image 0");
    assert_eq!(f.entries(), ["in.gltf"]);
}
#[test]
fn undecodable_data_uris_are_errors() {
    for (uri, message) in [
        ("data:application/octet-stream,AAAA", "Only base64"),
        (
            "data:application/octet-stream;base64,A!AA",
            "Invalid base64",
        ),
        ("data:application/octet-stream;base64,", "Empty data"),
        ("data:application/octet-stream;base64", "Malformed data"),
    ] {
        let f = imported_gltf(|root| root["buffers"][0]["uri"] = json!(uri));
        let out = f.run(&["inspect"], "in.gltf");
        assert!(!out.status.success(), "{uri}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(message), "{uri}: {stderr}");
        assert!(stderr.contains("Buffer 0"), "{stderr}");
    }
}
/// Make each UV accessor sparse: `indices` of its elements, encoded as `component`, take the
/// values `map` gives their current UV. Without `keep_base` the other elements read as zeros.
fn sparse_uvs(
    component: u64,
    indices: &[u32],
    keep_base: bool,
    map: impl Fn([f32; 2]) -> [f32; 2],
) -> Fixture {
    let f = imported_gltf(|_| {});
    let mut root = read_gltf(&f);
    let mut bin = fs::read(f.0.join("in.bin")).unwrap();
    let uvs: Vec<u64> = root["meshes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            m["primitives"][0]["attributes"]["TEXCOORD_0"]
                .as_u64()
                .unwrap()
        })
        .collect();
    for uv in uvs {
        let accessor = root["accessors"][uv as usize].clone();
        let view = &root["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
        let start = (view["byteOffset"].as_u64().unwrap()
            + accessor["byteOffset"].as_u64().unwrap()) as usize;
        let stride = view["byteStride"].as_u64().unwrap() as usize;
        let (mut encoded, mut values) = (Vec::new(), Vec::new());
        for &i in indices {
            match component {
                5121 => encoded.push(i as u8),
                5123 => encoded.extend((i as u16).to_le_bytes()),
                _ => encoded.extend(i.to_le_bytes()),
            }
            // An index outside the accessor is only there to be rejected.
            let at = start + i as usize * stride;
            let number = |o: usize| {
                bin.get(o..o + 4)
                    .map_or(0., |b| f32::from_le_bytes(b.try_into().unwrap()))
            };
            for value in map([number(at), number(at + 4)]) {
                values.extend(value.to_le_bytes());
            }
        }
        let views = root["bufferViews"].as_array().unwrap().len();
        for section in [&encoded, &values] {
            bin.resize(bin.len().next_multiple_of(4), 0);
            root["bufferViews"]
                .as_array_mut()
                .unwrap()
                .push(json!({"buffer": 0, "byteOffset": bin.len(), "byteLength": section.len()}));
            bin.extend(section);
        }
        let target = root["accessors"][uv as usize].as_object_mut().unwrap();
        if !keep_base {
            target.remove("bufferView");
            target.remove("byteOffset");
        }
        target.insert(
            "sparse".into(),
            json!({
                "count": indices.len(),
                "indices": {"bufferView": views, "componentType": component},
                "values": {"bufferView": views + 1},
            }),
        );
    }
    root["buffers"][0]["byteLength"] = json!(bin.len());
    f.file("in.gltf", &root.to_string());
    fs::write(f.0.join("in.bin"), bin).unwrap();
    f
}
#[test]
fn sparse_accessors_replace_elements_of_their_base() {
    let f = half_textured("source.png");
    assert!(f.run(&["import"], "in.obj").status.success());
    let plain = f.inspect_json("in.gltf");
    // The same values, over a base or over zeros, change nothing.
    for component in [5121, 5123, 5125] {
        let same = sparse_uvs(component, &[0, 1, 2, 3], false, |uv| uv);
        assert_eq!(same.inspect_json("in.gltf"), plain, "{component}");
    }
    // Elements left out keep their base values.
    let partial = sparse_uvs(5123, &[1, 3], true, |uv| uv);
    assert_eq!(partial.inspect_json("in.gltf"), plain);
    // New values take effect: the quad's UVs now span the whole texture.
    let spread = |[u, v]: [f32; 2]| [(u - 0.25) * 4., (v - 0.25) * 2.];
    let whole = sparse_uvs(5125, &[0, 1, 2, 3], true, spread);
    assert_eq!(whole.inspect_json("in.gltf")["occupied_texels"], 64);
}
#[test]
fn sparse_elements_left_out_of_a_view_less_accessor_read_as_zero() {
    // One corner keeps its UV and the others collapse on (0, 0), so a different region is used.
    let f = sparse_uvs(5123, &[2], false, |_| [0.5, 0.5]);
    let report = f.inspect_json("in.gltf");
    let occupied = report["occupied_texels"].as_u64().unwrap();
    assert!(occupied > 0 && occupied != 24, "{occupied}");
}
#[test]
fn invalid_sparse_accessors_are_errors() {
    for indices in [&[1, 1][..], &[2, 1], &[0, 4], &[0, 1, 2, 3, 3]] {
        let f = sparse_uvs(5123, indices, true, |uv| uv);
        let out = f.run(&["inspect"], "in.gltf");
        assert!(!out.status.success(), "{indices:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("Sparse") || stderr.contains("sparse"),
            "{stderr}"
        );
    }
    let f = sparse_uvs(5126, &[0], true, |uv| uv);
    let out = f.run(&["inspect"], "in.gltf");
    assert!(String::from_utf8_lossy(&out.stderr).contains("unsigned integers"));
    assert!(!out.status.success());
}
#[test]
fn accessors_without_data_read_as_zeros() {
    // The spec's rule, without a Draco extension in the way: every corner lands on one spot.
    let f = imported_gltf(|root| {
        let uvs: Vec<_> = root["meshes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                m["primitives"][0]["attributes"]["TEXCOORD_0"]
                    .as_u64()
                    .unwrap() as usize
            })
            .collect();
        for uv in uvs {
            let accessor = root["accessors"][uv].as_object_mut().unwrap();
            accessor.remove("bufferView");
            accessor.remove("byteOffset");
        }
    });
    // Linear filtering around (0, 0) reads one texel in each corner of the wrapping texture.
    assert_eq!(f.inspect_json("in.gltf")["occupied_texels"], 4);
}
