use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
static SEQ: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "objtools-integration-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn file(&self, name: &str, body: &str) -> PathBuf {
        let p = self.0.join(name);
        fs::write(&p, body).unwrap();
        p
    }
    fn import(&self, input: &str, center: bool) -> std::process::Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_objtools"));
        cmd.arg("import")
            .arg(self.0.join(input))
            .arg("-o")
            .arg(self.0.join("out.gltf"))
            .arg("--tmp-dir")
            .arg(&self.0);
        if center {
            cmd.arg("--center");
        }
        cmd.output().unwrap()
    }
    fn result(&self) -> (Value, Vec<u8>) {
        (
            serde_json::from_slice(&fs::read(self.0.join("out.gltf")).unwrap()).unwrap(),
            fs::read(self.0.join("out.bin")).unwrap(),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn floats(root: &Value, bin: &[u8], accessor: usize, components: usize) -> Vec<Vec<f32>> {
    let a = &root["accessors"][accessor];
    let v = &root["bufferViews"][a["bufferView"].as_u64().unwrap() as usize];
    let offset = v["byteOffset"].as_u64().unwrap_or(0) as usize
        + a["byteOffset"].as_u64().unwrap_or(0) as usize;
    let stride = v["byteStride"].as_u64().unwrap_or((components * 4) as u64) as usize;
    (0..a["count"].as_u64().unwrap() as usize)
        .map(|i| {
            (0..components)
                .map(|j| {
                    f32::from_le_bytes(
                        bin[offset + i * stride + j * 4..offset + i * stride + j * 4 + 4]
                            .try_into()
                            .unwrap(),
                    )
                })
                .collect()
        })
        .collect()
}
#[test]
fn exact_center_scientific_notation_and_ranges() {
    let f = Fixture::new();
    f.file("in.obj","o georeferenced\nv 6.512345678901234e5 6862000 120\nv 651234.5698901234 6862000 120\nv 651234.5678901234 6862000.002 120\nf -3 -2 -1\n");
    let result = f.import("in.obj", true);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, bin) = f.result();
    assert_eq!(root["extras"]["objtools"]["origin"][0], "651234.5688901234");
    assert_eq!(
        root["buffers"][0]["byteLength"].as_u64().unwrap(),
        bin.len() as u64
    );
    let primitive = &root["meshes"][0]["primitives"][0];
    let p = floats(
        &root,
        &bin,
        primitive["attributes"]["POSITION"].as_u64().unwrap() as usize,
        3,
    );
    assert!((p[0][0] + 0.001).abs() < 1e-8);
    assert!((p[1][0] - 0.001).abs() < 1e-8);
    let normals = floats(
        &root,
        &bin,
        primitive["attributes"]["NORMAL"].as_u64().unwrap() as usize,
        3,
    );
    assert_eq!(normals[0], vec![0., 0., 1.]);
    for view in root["bufferViews"].as_array().unwrap() {
        let start = view["byteOffset"].as_u64().unwrap();
        let length = view["byteLength"].as_u64().unwrap();
        assert_eq!(start % 4, 0);
        assert!(start + length <= bin.len() as u64);
    }
}
#[test]
fn seams_materials_and_explicit_meshes() {
    let f = Fixture::new();
    f.file("in.mtl","newmtl paint\nKd 0.3 0.4 0.5\nKs 0.2 0.1 0.04\nNs 126\nTr 0.8\nd 0.7\nnewmtl metal\nPm 1\nPr 0.2\n");
    f.file("in.obj","mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 0 1\nvt .5 .5\nvn 0 0 2\no first\nusemtl paint\nf 1/1/1 2/2/1 3/3/1\nf 1/4/1 2/2/1 3/3/1\no second\nusemtl metal\nf 1/1/1 2/2/1 3/3/1\n");
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, bin) = f.result();
    assert_eq!(root["meshes"].as_array().unwrap().len(), 2);
    let p = &root["meshes"][0]["primitives"][0];
    let accessor = p["attributes"]["POSITION"].as_u64().unwrap() as usize;
    assert_eq!(root["accessors"][accessor]["count"], 4);
    let uv = floats(
        &root,
        &bin,
        p["attributes"]["TEXCOORD_0"].as_u64().unwrap() as usize,
        2,
    );
    assert_eq!(uv[0], vec![0., 1.]);
    let paint = &root["materials"][p["material"].as_u64().unwrap() as usize];
    assert_eq!(paint["pbrMetallicRoughness"]["metallicFactor"], 0.);
    assert_eq!(paint["alphaMode"], "BLEND");
    assert_eq!(paint["pbrMetallicRoughness"]["baseColorFactor"][3], 0.7);
    let a = &root["meshes"][1]["primitives"][0];
    let metal = &root["materials"][a["material"].as_u64().unwrap() as usize];
    assert_eq!(metal["pbrMetallicRoughness"]["metallicFactor"], 1.);
}
/// Each primitive's vertex count and index component type, in output order.
fn primitive_shapes(root: &Value) -> Vec<(u64, u64)> {
    root["meshes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["primitives"].as_array().unwrap())
        .map(|p| {
            let accessor = |id: &Value| &root["accessors"][id.as_u64().unwrap() as usize];
            (
                accessor(&p["attributes"]["POSITION"])["count"]
                    .as_u64()
                    .unwrap(),
                accessor(&p["indices"])["componentType"].as_u64().unwrap(),
            )
        })
        .collect()
}
#[test]
fn repeated_triangles_share_one_primitive_and_vertices() {
    let f = Fixture::new();
    let mut file = fs::File::create(f.0.join("in.obj")).unwrap();
    writeln!(file, "v 0 0 0\nv 1 0 0\nv 0 1 0\no huge").unwrap();
    for _ in 0..65537 {
        writeln!(file, "f 1 2 3").unwrap();
    }
    drop(file);
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, _) = f.result();
    assert_eq!(primitive_shapes(&root), [(3, 5123)]);
    let p = &root["meshes"][0]["primitives"][0];
    let indices = &root["accessors"][p["indices"].as_u64().unwrap() as usize];
    assert_eq!(indices["count"], 65537 * 3);
}
/// A fan of `n - 1` triangles around an apex, using all `n` positions.
fn fan(f: &Fixture, n: usize) {
    let mut file = fs::File::create(f.0.join("in.obj")).unwrap();
    writeln!(file, "v 0 0 1").unwrap();
    for k in 1..n {
        writeln!(file, "v {k} 0 0").unwrap();
    }
    for k in 2..n {
        writeln!(file, "f 1 {k} {}", k + 1).unwrap();
    }
}
#[test]
fn index_width_follows_vertex_count() {
    // 65,535 vertices still fit 16-bit indices: 65,535 itself is the reserved restart value.
    for (n, component) in [(65535, 5123), (65536, 5125)] {
        let f = Fixture::new();
        fan(&f, n);
        let result = f.import("in.obj", false);
        assert!(result.status.success());
        let (root, bin) = f.result();
        assert_eq!(primitive_shapes(&root), [(n as u64, component)]);
        assert_eq!(
            root["buffers"][0]["byteLength"].as_u64().unwrap(),
            bin.len() as u64
        );
        for view in root["bufferViews"].as_array().unwrap() {
            assert_eq!(view["byteOffset"].as_u64().unwrap() % 4, 0);
        }
    }
}
#[test]
fn shuffled_faces_and_interleaved_materials_keep_exact_vertex_counts() {
    // A 40 x 40 grid of quads in a scrambled order, alternating two materials every quad.
    let f = Fixture::new();
    f.file("in.mtl", "newmtl a\nnewmtl b\n");
    let mut file = fs::File::create(f.0.join("in.obj")).unwrap();
    writeln!(file, "mtllib in.mtl").unwrap();
    for y in 0..41 {
        for x in 0..41 {
            writeln!(file, "v {x} {y} 0").unwrap();
        }
    }
    let at = |x: usize, y: usize| y * 41 + x + 1;
    let mut corners = [
        std::collections::BTreeSet::new(),
        std::collections::BTreeSet::new(),
    ];
    for i in 0..1600 {
        let q = (i * 997) % 1600;
        let (x, y) = (q % 40, q / 40);
        let material = (x + y) % 2;
        let quad = [at(x, y), at(x + 1, y), at(x + 1, y + 1), at(x, y + 1)];
        corners[material].extend(quad);
        writeln!(file, "usemtl {}", ["a", "b"][material]).unwrap();
        writeln!(file, "f {} {} {} {}", quad[0], quad[1], quad[2], quad[3]).unwrap();
    }
    drop(file);
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, _) = f.result();
    // One primitive per material, in order of first use, each holding exactly the positions
    // its faces use. The first face is the corner quad, which uses material a.
    let shapes = primitive_shapes(&root);
    assert_eq!(shapes.len(), 2);
    let material = |i: usize| {
        root["meshes"][0]["primitives"][i]["material"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(root["materials"][material(0) as usize]["name"], "a");
    for (i, shape) in shapes.iter().enumerate() {
        let name = &root["materials"][material(i) as usize]["name"];
        let set = &corners[if name == "a" { 0 } else { 1 }];
        assert_eq!(*shape, (set.len() as u64, 5123));
    }
}
#[test]
fn vrml_material_defaults_and_georeferencing() {
    let f = Fixture::new();
    f.file("in.wrl","#VRML V2.0 utf8\nDEF terrain Shape { appearance Appearance { material Material {} } geometry IndexedFaceSet { coordIndex [0,1,2,-1] coord Coordinate { point [651000 6862000 120,651001 6862000 120,651000 6862001 120] } } }");
    let result = f.import("in.wrl", true);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, _) = f.result();
    assert_eq!(root["meshes"][0]["name"], "terrain");
    let p = &root["meshes"][0]["primitives"][0];
    let m = &root["materials"][p["material"].as_u64().unwrap() as usize];
    assert_eq!(
        m["extras"]["sourcePhong"]["Ks"],
        serde_json::json!([0., 0., 0.])
    );
    assert_eq!(m["extras"]["sourcePhong"]["Ns"], 25.6);
}
#[test]
fn errors_leave_no_output_and_do_not_overwrite() {
    let f = Fixture::new();
    f.file("in.obj", "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 9\n");
    assert!(!f.import("in.obj", true).status.success());
    assert!(!f.0.join("out.bin").exists());
    assert!(!f.0.join("out.gltf").exists());
    f.file("out.bin", "existing");
    assert!(!f.import("in.obj", false).status.success());
    assert_eq!(fs::read_to_string(f.0.join("out.bin")).unwrap(), "existing");
    let result = Command::new(env!("CARGO_BIN_EXE_objtools"))
        .args(["split", "in.obj"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    let result = Command::new(env!("CARGO_BIN_EXE_objtools"))
        .args(["import", "--typo"])
        .output()
        .unwrap();
    assert!(!result.status.success());
}
#[test]
fn workspace_defaults_to_output_folder_and_is_removed() {
    let f = Fixture::new();
    f.file("in.obj", "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n");
    let entries = |dir: &str| {
        let mut names: Vec<String> = fs::read_dir(f.0.join(dir))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    };
    fs::create_dir(f.0.join("clean")).unwrap();
    assert!(import_images(&f, "clean/out.gltf", &[]).status.success());
    assert_eq!(entries("clean"), ["out.bin", "out.gltf"]);
    fs::create_dir(f.0.join("kept")).unwrap();
    assert!(import_images(&f, "kept/out.gltf", &["--keep-tmp"])
        .status
        .success());
    let kept = entries("kept");
    assert_eq!(kept.len(), 3);
    assert!(f.0.join("kept").join(&kept[0]).join("faces").is_file());
}
#[test]
fn unsupported_vrml_transform_is_an_error() {
    let f = Fixture::new();
    f.file(
        "in.wrl",
        "#VRML V2.0 utf8\nTransform { translation 651000 6862000 0 children [] }",
    );
    let result = f.import("in.wrl", true);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Non-identity"));
}

#[test]
fn z_up_rotation_and_texture_publication() {
    let f = Fixture::new();
    // One translucent texel is enough to need blending.
    image::RgbaImage::from_fn(2, 2, |x, y| {
        image::Rgba([9, 9, 9, if x + y == 2 { 254 } else { 255 }])
    })
    .save(f.0.join("albedo.png"))
    .unwrap();
    let png = fs::read(f.0.join("albedo.png")).unwrap();
    f.file("in.mtl", "newmtl paint\nKd 1 1 1\nmap_Kd albedo.png\n");
    f.file("in.obj", "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 0 1\nusemtl paint\nf 1/1 2/2 3/3\n");
    let result = Command::new(env!("CARGO_BIN_EXE_objtools"))
        .arg("import")
        .arg(f.0.join("in.obj"))
        .args(["--up-axis", "z", "--images", "copy", "-o"])
        .arg(f.0.join("with spaces.gltf"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let root: Value =
        serde_json::from_slice(&fs::read(f.0.join("with spaces.gltf")).unwrap()).unwrap();
    assert_eq!(root["extras"]["objtools"]["sourceUpAxis"], "z");
    assert!(
        (root["nodes"][0]["rotation"][0].as_f64().unwrap() + std::f64::consts::FRAC_1_SQRT_2).abs()
            < 1e-15
    );
    assert_eq!(root["buffers"][0]["uri"], "with%20spaces.bin");
    assert_eq!(root["images"][0]["uri"], "with%20spaces_texture_0.png");
    assert_eq!(root["materials"][0]["alphaMode"], "BLEND");
    assert_eq!(
        fs::read(f.0.join("with spaces_texture_0.png")).unwrap(),
        png
    );
}

#[test]
fn opaque_alpha_channel_keeps_material_opaque() {
    let f = Fixture::new();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([9, 9, 9, 255]))
        .save(f.0.join("albedo.png"))
        .unwrap();
    f.file("in.mtl", "newmtl paint\nmap_Kd albedo.png\n");
    f.file(
        "in.obj",
        "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl paint\nf 1/1 2/1 3/1\n",
    );
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, _) = f.result();
    assert_eq!(root["materials"][0]["alphaMode"], "OPAQUE");
}

#[test]
fn texture_error_cleans_previously_copied_files() {
    let f = Fixture::new();
    image::RgbaImage::new(1, 1)
        .save(f.0.join("valid.png"))
        .unwrap();
    f.file(
        "in.mtl",
        "newmtl first\nmap_Kd valid.png\nnewmtl missing\nmap_Kd absent.png\n",
    );
    f.file("in.obj", "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl first\nf 1/1 2/1 3/1\nusemtl missing\nf 1/1 2/1 3/1\n");
    let result = f.import("in.obj", false);
    assert!(!result.status.success());
    assert!(!f.0.join("out_texture_0.png").exists());
    assert!(!f.0.join("out.gltf").exists());
    assert!(!f.0.join("out.bin").exists());
}

#[test]
fn smooth_normals_ignore_s_off_and_cross_material_and_uv_seams() {
    let f = Fixture::new();
    f.file("in.mtl", "newmtl first\nnewmtl second\n");
    f.file("in.obj", "mtllib in.mtl\ns off\nv 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 2\nvt 0 0\nvt .5 .5\nusemtl first\nf 1/1 2/1 3/1\nusemtl second\nf 1/2 4/2 2/2\n");
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!String::from_utf8_lossy(&result.stderr).contains("smoothing groups"));
    let (root, bin) = f.result();
    let primitives = root["meshes"][0]["primitives"].as_array().unwrap();
    assert_eq!(primitives.len(), 2);
    for primitive in primitives {
        let normals = floats(
            &root,
            &bin,
            primitive["attributes"]["NORMAL"].as_u64().unwrap() as usize,
            3,
        );
        let shared = &normals[0];
        assert!(shared[0].abs() < 1e-6);
        assert!((shared[1] - 2. / 5f32.sqrt()).abs() < 1e-6);
        assert!((shared[2] - 1. / 5f32.sqrt()).abs() < 1e-6);
    }
}

#[test]
fn smooth_default_preserves_authored_normals() {
    let f = Fixture::new();
    f.file(
        "in.obj",
        "v 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\nvn 2 0 0\nf 1//1 2//1 3//1\nf 1 4 2\n",
    );
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, bin) = f.result();
    let primitive = &root["meshes"][0]["primitives"][0];
    let normals = floats(
        &root,
        &bin,
        primitive["attributes"]["NORMAL"].as_u64().unwrap() as usize,
        3,
    );
    assert_eq!(normals[0], vec![1., 0., 0.]);
    assert!((normals[3][1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    assert!((normals[3][2] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
}

#[test]
fn vrml_defaults_to_smooth_normals_with_zero_crease_angle() {
    let f = Fixture::new();
    f.file("in.wrl", "#VRML V2.0 utf8\nShape { geometry IndexedFaceSet { creaseAngle 0 coord Coordinate { point [0 0 0,1 0 0,0 1 0,0 0 1] } coordIndex [0,1,2,-1,0,3,1,-1] } }");
    let result = f.import("in.wrl", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, bin) = f.result();
    let primitive = &root["meshes"][0]["primitives"][0];
    let normals = floats(
        &root,
        &bin,
        primitive["attributes"]["NORMAL"].as_u64().unwrap() as usize,
        3,
    );
    assert!((normals[0][1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    assert!((normals[0][2] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
}

#[test]
fn smooth_normals_on_a_primitive_with_32_bit_indices() {
    let f = Fixture::new();
    let mut source = fs::File::create(f.0.join("in.obj")).unwrap();
    for i in 0..22000 {
        writeln!(
            source,
            "v {} 0 0\nv {} 0 0\nv {} 1 0",
            i * 3,
            i * 3 + 1,
            i * 3
        )
        .unwrap();
    }
    writeln!(source, "v 0 0 1").unwrap();
    for i in 0..22000 {
        writeln!(source, "f {} {} {}", i * 3 + 1, i * 3 + 2, i * 3 + 3).unwrap();
    }
    // Revisit positions 1 and 2 after 66,000 other vertices.
    writeln!(source, "f 1 66001 2").unwrap();
    drop(source);
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, bin) = f.result();
    assert_eq!(primitive_shapes(&root), [(66001, 5125)]);
    let primitive = &root["meshes"][0]["primitives"][0];
    let attribute = |name: &str| primitive["attributes"][name].as_u64().unwrap() as usize;
    let positions = floats(&root, &bin, attribute("POSITION"), 3);
    let normals = floats(&root, &bin, attribute("NORMAL"), 3);
    let mut occurrences = 0;
    for (position, normal) in positions.iter().zip(normals) {
        if position == &[0., 0., 0.] {
            occurrences += 1;
            assert!((normal[1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
            assert!((normal[2] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        }
    }
    assert_eq!(occurrences, 1);
}
fn textured_fixture(extension: &str) -> Fixture {
    let f = Fixture::new();
    let image = image::RgbImage::from_fn(4, 4, |x, y| {
        image::Rgb([(x * 60) as u8, (y * 60) as u8, 128])
    });
    image.save(f.0.join(format!("source.{extension}"))).unwrap();
    f.file(
        "in.mtl",
        &format!("newmtl paint\nmap_Kd source.{extension}\n"),
    );
    f.file(
        "in.obj",
        "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl paint\nf 1/1 2/1 3/1\n",
    );
    f
}
fn import_images(f: &Fixture, output: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_objtools"))
        .arg("import")
        .arg(f.0.join("in.obj"))
        .arg("-o")
        .arg(f.0.join(output))
        .args(args)
        .output()
        .unwrap()
}
fn image_uri(f: &Fixture, output: &str) -> String {
    let root: Value = serde_json::from_slice(&fs::read(f.0.join(output)).unwrap()).unwrap();
    root["images"][0]["uri"].as_str().unwrap().to_owned()
}
#[test]
fn image_defaults_keep_in_same_folder_and_copy_in_different_folder() {
    let f = textured_fixture("png");
    let result = import_images(&f, "same.gltf", &[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(image_uri(&f, "same.gltf"), "source.png");
    assert!(!f.0.join("same_texture_0.png").exists());
    fs::create_dir(f.0.join("destination")).unwrap();
    let result = import_images(&f, "destination/out.gltf", &[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(image_uri(&f, "destination/out.gltf"), "out_texture_0.png");
    assert_eq!(
        fs::read(f.0.join("source.png")).unwrap(),
        fs::read(f.0.join("destination/out_texture_0.png")).unwrap()
    );
    let result = import_images(&f, "destination/keep.gltf", &["--images", "keep"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(image_uri(&f, "destination/keep.gltf"), "../source.png");
}
#[test]
fn unsupported_image_defaults_to_jpg_conversion_and_false_aborts() {
    let f = textured_fixture("bmp");
    let original = fs::read(f.0.join("source.bmp")).unwrap();
    let result = import_images(&f, "converted.gltf", &[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(image_uri(&f, "converted.gltf"), "converted_texture_0.jpg");
    let image = image::open(f.0.join("converted_texture_0.jpg")).unwrap();
    assert_eq!((image.width(), image.height()), (4, 4));
    assert_eq!(fs::read(f.0.join("source.bmp")).unwrap(), original);
    let result = import_images(&f, "disabled.gltf", &["--convert-images=false"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr)
        .contains("unsupported glTF image format .bmp with --convert-images=false"));
    assert!(!f.0.join("disabled.gltf").exists());
    assert!(!f.0.join("disabled.bin").exists());
}
#[test]
fn forced_conversion_selects_png_and_jpg_quality() {
    let f = textured_fixture("png");
    let result = import_images(
        &f,
        "jpeg.gltf",
        &["--convert-images", "true", "--image-quality", "95"],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(image_uri(&f, "jpeg.gltf"), "jpeg_texture_0.jpg");
    let result = import_images(
        &f,
        "png.gltf",
        &[
            "--convert-images=true",
            "--image-format",
            "png",
            "--images",
            "keep",
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(image_uri(&f, "png.gltf"), "png_texture_0.png");
    assert_eq!(
        image::open(f.0.join("source.png")).unwrap().to_rgb8(),
        image::open(f.0.join("png_texture_0.png"))
            .unwrap()
            .to_rgb8()
    );
    let result = import_images(&f, "bad.gltf", &["--image-quality", "101"]);
    assert!(!result.status.success());
}
#[test]
fn png_conversion_preserves_alpha_and_jpg_composites_white() {
    let f = textured_fixture("tga");
    image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 0]))
        .save(f.0.join("source.tga"))
        .unwrap();
    let result = import_images(&f, "png.gltf", &["--image-format", "png"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        image::open(f.0.join("png_texture_0.png"))
            .unwrap()
            .to_rgba8()
            .get_pixel(0, 0)[3],
        0
    );
    let result = import_images(&f, "jpg.gltf", &[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("composites transparency onto white"));
    assert!(image::open(f.0.join("jpg_texture_0.jpg"))
        .unwrap()
        .to_rgb8()
        .get_pixel(0, 0)
        .0
        .iter()
        .all(|c| *c >= 250));
}

#[test]
fn awkward_polygons_triangulate_instead_of_aborting() {
    let f = Fixture::new();
    // A triangle written as a quad, a keyhole (a square with a square hole), four collinear
    // corners, and a self-intersecting pentagon.
    f.file(
        "in.obj",
        "v 0 0 0\nv 4 0 0\nv 4 4 0\nv 0 4 0\nv 1 1 0\nv 1 3 0\nv 3 3 0\nv 3 1 0\n\
         v 1 0 0\nv 2 0 0\nv 3 0 0\n\
         v 1 2 1\nv 2 2 1\nv 1 3 1\nv 4 0 1\nv 0 3 1\n\
         f 1 2 3 3\nf 1 2 3 4 1 5 6 7 8 5\nf 1 9 10 11\nf 12 13 14 15 16\n",
    );
    let result = f.import("in.obj", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(
        stderr.contains("skipped 1 faces with zero area"),
        "{stderr}"
    );
    assert!(
        stderr.contains("triangulated 1 self-intersecting"),
        "{stderr}"
    );
    let (root, _) = f.result();
    let p = &root["meshes"][0]["primitives"][0];
    // 1 + 8 + 2: the pentagon's third triangle has no area.
    assert_eq!(
        root["accessors"][p["indices"].as_u64().unwrap() as usize]["count"],
        3 * 11
    );
}

fn mesh_names(root: &Value) -> Vec<&str> {
    root["meshes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect()
}

#[test]
fn groups_are_objects_until_the_first_o() {
    let f = Fixture::new();
    // A group-only file: an empty header group, a nameless group, and a repeated name.
    f.file(
        "in.obj",
        "g header\nv 0 0 0\nv 1 0 0\nv 0 1 0\ng a\nf 1 2 3\ng b\nf 1 2 3\ng\nf 1 2 3\ng a\nf 1 2 3\n",
    );
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, _) = f.result();
    assert_eq!(mesh_names(&root), ["a", "b", "default", "a"]);
    let nodes: Vec<_> = root["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["name"].as_str())
        .collect();
    assert_eq!(nodes, ["a", "b", "default", "a"]);
    // Once an `o` names objects, groups no longer split them.
    let f = Fixture::new();
    f.file(
        "in.obj",
        "v 0 0 0\nv 1 0 0\nv 0 1 0\ng lead\nf 1 2 3\no house\ng roof\nf 1 2 3\ng walls\nf 1 2 3\n",
    );
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, _) = f.result();
    assert_eq!(mesh_names(&root), ["lead", "house"]);
    let house = &root["meshes"][1]["primitives"][0];
    assert_eq!(
        root["accessors"][house["indices"].as_u64().unwrap() as usize]["count"],
        6
    );
}

#[test]
fn faces_without_uvs_get_an_untextured_copy_of_their_material() {
    let f = textured_fixture("png");
    f.file("in.mtl", "newmtl paint\nKd 0.5 0.25 1\nmap_Kd source.png\n");
    f.file(
        "in.obj",
        "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl paint\nf 1/1 2/1 3/1\nf 1 2 3\nf 3 2 1\n",
    );
    let result = f.import("in.obj", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(
        stderr.contains("2 faces with a textured material have no UVs"),
        "{stderr}"
    );
    let (root, _) = f.result();
    let primitives = root["meshes"][0]["primitives"].as_array().unwrap();
    assert_eq!(primitives.len(), 2);
    for p in primitives {
        let material = &root["materials"][p["material"].as_u64().unwrap() as usize];
        let pbr = &material["pbrMetallicRoughness"];
        assert_eq!(
            pbr["baseColorFactor"],
            serde_json::json!([0.5, 0.25, 1., 1.])
        );
        if p["attributes"].get("TEXCOORD_0").is_some() {
            assert_eq!(material["name"], "paint");
            assert!(pbr.get("baseColorTexture").is_some());
        } else {
            assert_eq!(material["name"], "paint-untextured");
            assert!(pbr.get("baseColorTexture").is_none());
        }
    }
    assert_eq!(root["materials"].as_array().unwrap().len(), 2);
    assert!(root.get("extensionsRequired").is_none());
}

#[test]
fn trailing_backslash_continues_statements_but_not_comments() {
    let f = Fixture::new();
    // A comment ending in a backslash keeps the next vertex; a face spans three lines, with
    // whitespace after a backslash; the last face ends the file mid-continuation.
    f.file(
        "in.obj",
        "# exported to C:\\models\\\nv 0 0 0\nv 1 0 0\nv 1 1 \\\n 0\nv 0 1 0\nf 1 2 \\  \n 3 \\\n 4\nf 1 2 3 \\",
    );
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, _) = f.result();
    let p = &root["meshes"][0]["primitives"][0];
    assert_eq!(
        root["accessors"][p["attributes"]["POSITION"].as_u64().unwrap() as usize]["count"],
        4
    );
    assert_eq!(
        root["accessors"][p["indices"].as_u64().unwrap() as usize]["count"],
        9
    );
    // Errors name the line a continued statement starts on.
    let f = Fixture::new();
    f.file("bad.obj", "v 0 0 0\nf 1 \\\n 2 x\n");
    let result = f.import("bad.obj", false);
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("bad.obj:2:"), "{stderr}");
}

#[test]
fn map_kd_options_become_texture_transform_and_clamp() {
    let f = Fixture::new();
    image::RgbImage::new(2, 2)
        .save(f.0.join("wood grain.png"))
        .unwrap();
    f.file(
        "in.mtl",
        "newmtl plain\nmap_Kd -s 1 1 1 -o 0 0 0 -blendu off -cc on wood grain.png\n\
         newmtl tiled\nmap_Kd -s 2 4 1 -o 0.25 -0.5 0 -clamp on -bm 1 -imfchan r -texres 512 -mm 0 2 -t 0.1 wood grain.png\n",
    );
    f.file(
        "in.obj",
        "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl plain\nf 1/1 2/1 3/1\nusemtl tiled\nf 1/1 2/1 3/1\n",
    );
    let result = f.import("in.obj", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(stderr.contains("map_Kd -mm is ignored"), "{stderr}");
    assert!(stderr.contains("map_Kd -t is ignored"), "{stderr}");
    let (root, _) = f.result();
    // One image, sampled by two textures: repeat for `plain`, clamp for `tiled`.
    assert_eq!(root["images"].as_array().unwrap().len(), 1);
    assert_eq!(
        root["samplers"],
        serde_json::json!([{"wrapS":10497,"wrapT":10497},{"wrapS":33071,"wrapT":33071}])
    );
    let info = |m: usize| &root["materials"][m]["pbrMetallicRoughness"]["baseColorTexture"];
    let sampler =
        |m: usize| root["textures"][info(m)["index"].as_u64().unwrap() as usize]["sampler"].clone();
    assert_eq!(root["materials"][0]["name"], "plain");
    assert_eq!(sampler(0), 0);
    assert_eq!(sampler(1), 1);
    // Identity options add nothing; `-s 2 4 -o 0.25 -0.5` samples at 2u + 0.25 and, with V
    // flipped, 4v + 1 - 4 + 0.5.
    assert!(info(0).get("extensions").is_none());
    assert_eq!(
        info(1)["extensions"]["KHR_texture_transform"],
        serde_json::json!({"offset": [0.25, -2.5], "scale": [2., 4.]})
    );
    assert_eq!(
        root["extensionsUsed"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| *e == "KHR_texture_transform")
            .count(),
        1
    );
    assert_eq!(
        root["extensionsRequired"],
        serde_json::json!(["KHR_texture_transform"])
    );
    // An option outside the spec's list leaves no way to find the path.
    let f = Fixture::new();
    f.file("in.mtl", "newmtl x\nmap_Kd -zoom 2 a.png\n");
    f.file("in.obj", "mtllib in.mtl\nv 0 0 0\n");
    let result = f.import("in.obj", false);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Unsupported map_Kd option -zoom"));
}

#[test]
fn faces_with_fewer_than_three_corners_are_omitted() {
    let f = Fixture::new();
    f.file(
        "in.obj",
        "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2\nf 3\nf\nf 1 2 3\n",
    );
    let result = f.import("in.obj", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(
        stderr.contains("omitted 3 faces with fewer than three corners"),
        "{stderr}"
    );
    let (root, _) = f.result();
    let p = &root["meshes"][0]["primitives"][0];
    assert_eq!(
        root["accessors"][p["indices"].as_u64().unwrap() as usize]["count"],
        3
    );
    // Their indices are still checked.
    f.file("bad.obj", "v 0 0 0\nf 1 2\n");
    assert!(!f.import("bad.obj", false).status.success());
}

#[test]
fn zero_source_normals_are_smoothed_and_degenerate_triangles_skipped() {
    let f = Fixture::new();
    f.file("in.obj","v 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\nvn 0 0 0\nf 1//1 2//1 3//1\nf 1//1 4//1 2//1\nf 1//1 1//1 2//1\n");
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("zero-length source normals"));
    assert!(String::from_utf8_lossy(&result.stderr).contains("skipped 1 faces with zero area"));
    let (root, bin) = f.result();
    let p = &root["meshes"][0]["primitives"][0];
    assert_eq!(
        root["accessors"][p["indices"].as_u64().unwrap() as usize]["count"],
        6
    );
    let normals = floats(
        &root,
        &bin,
        p["attributes"]["NORMAL"].as_u64().unwrap() as usize,
        3,
    );
    assert!((normals[0][1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    assert!((normals[0][2] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
}

#[test]
fn vertex_colors_become_linear_color_0_except_under_textures() {
    let f = Fixture::new();
    image::RgbImage::new(1, 1)
        .save(f.0.join("albedo.png"))
        .unwrap();
    f.file(
        "in.mtl",
        "newmtl plain\nKd 1 1 1\nnewmtl photo\nmap_Kd albedo.png\n",
    );
    // The first position has no colour: it is back-filled as white once colours appear.
    f.file("in.obj", "mtllib in.mtl\nv 0 0 0\nv 1 0 0 1 0.5 0\nv 0 1 0 0.2 0.04045 1 0.5\nvt 0 0\nvt 1 0\nvt 0 1\nusemtl plain\nf 1 2 3\nf 1/1 2/2 3/3\nusemtl photo\nf 1/1 2/2 3/3\n");
    let result = f.import("in.obj", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert!(stderr.contains("vertex color alpha is omitted"), "{stderr}");
    assert!(
        stderr.contains("omitted vertex colors on 1 textured faces"),
        "{stderr}"
    );
    let (root, bin) = f.result();
    let primitives = root["meshes"][0]["primitives"].as_array().unwrap();
    assert_eq!(primitives.len(), 3);
    // Position+normal+colour, then the same with UVs in between.
    for (primitive, stride) in primitives.iter().zip([36, 44]) {
        let accessor = primitive["attributes"]["COLOR_0"].as_u64().unwrap() as usize;
        assert_eq!(root["accessors"][accessor]["type"], "VEC3");
        let view = root["accessors"][accessor]["bufferView"].as_u64().unwrap() as usize;
        assert_eq!(root["bufferViews"][view]["byteStride"], stride);
        let colors = floats(&root, &bin, accessor, 3);
        assert_eq!(colors[0], vec![1., 1., 1.]);
        assert_eq!(colors[1][0], 1.);
        assert!((colors[1][1] - 0.21404114).abs() < 1e-6);
        assert_eq!(colors[1][2], 0.);
        assert!((colors[2][0] - 0.03310477).abs() < 1e-6);
        assert!((colors[2][1] - 0.04045 / 12.92).abs() < 1e-7);
        let position = primitive["attributes"]["POSITION"].as_u64().unwrap() as usize;
        assert_eq!(floats(&root, &bin, position, 3)[2], vec![0., 1., 0.]);
    }
    let uv = primitives[1]["attributes"]["TEXCOORD_0"].as_u64().unwrap() as usize;
    assert_eq!(floats(&root, &bin, uv, 2)[1], vec![1., 1.]);
    assert!(primitives[2]["attributes"]["COLOR_0"].is_null());
    assert!(primitives[2]["attributes"]["TEXCOORD_0"].is_u64());
}

#[test]
fn unportable_vertex_components_warn_instead_of_failing() {
    let f = Fixture::new();
    f.file("in.obj", "v 0 0 0 1\nv 2 0 0 0.5\nv 0 2 0 1 2 3 4 5\nv 0 0 2 -1 0.5 255\nvt 0 0 0\nvt 1 0 0.5\nvt 0 1\nvt 1 1\nf 1/1 2/2 3/3\nf 1/1 2/2 4/4\n");
    let result = f.import("in.obj", false);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    for warning in [
        "vertex weights (w) are omitted",
        "unrecognized extra v components are omitted",
        "vertex colors outside [0,1] are clamped",
        "texture coordinates beyond u v are omitted",
    ] {
        assert_eq!(stderr.matches(warning).count(), 1, "{warning}: {stderr}");
    }
    let (root, bin) = f.result();
    let primitive = &root["meshes"][0]["primitives"][0];
    let position = primitive["attributes"]["POSITION"].as_u64().unwrap() as usize;
    // The weight is not applied as a divisor.
    assert_eq!(floats(&root, &bin, position, 3)[1], vec![2., 0., 0.]);
    let uv = primitive["attributes"]["TEXCOORD_0"].as_u64().unwrap() as usize;
    assert_eq!(floats(&root, &bin, uv, 2)[1], vec![1., 1.]);
    let color = primitive["attributes"]["COLOR_0"].as_u64().unwrap() as usize;
    let colors = floats(&root, &bin, color, 3);
    assert_eq!(colors[2], vec![1., 1., 1.]);
    assert_eq!(colors[3][0], 0.);
    assert_eq!(colors[3][2], 1.);
}
