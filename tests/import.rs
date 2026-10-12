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
#[test]
fn repeated_triangles_force_bounded_chunks() {
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
    let p = root["meshes"][0]["primitives"].as_array().unwrap();
    assert_eq!(p.len(), 2);
    let counts: Vec<_> = p
        .iter()
        .map(|p| {
            root["accessors"][p["indices"].as_u64().unwrap() as usize]["count"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(counts, vec![65536 * 3, 3]);
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
    // Minimal valid PNG with an alpha channel; exporter only needs headers.
    let png = [
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 0, 0, 0, 0,
    ];
    fs::write(f.0.join("albedo.png"), png).unwrap();
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
fn texture_error_cleans_previously_copied_files() {
    let f = Fixture::new();
    fs::write(
        f.0.join("valid.png"),
        [
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 0, 0, 0, 0,
        ],
    )
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
fn smooth_normals_survive_chunk_boundaries() {
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
    // Revisit positions 1 and 2 after the bounded accumulator was flushed to disk.
    writeln!(source, "f 1 66001 2").unwrap();
    drop(source);
    let result = f.import("in.obj", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (root, bin) = f.result();
    let primitives = root["meshes"][0]["primitives"].as_array().unwrap();
    assert_eq!(primitives.len(), 2);
    let mut occurrences = 0;
    for primitive in primitives {
        let positions = floats(
            &root,
            &bin,
            primitive["attributes"]["POSITION"].as_u64().unwrap() as usize,
            3,
        );
        let normals = floats(
            &root,
            &bin,
            primitive["attributes"]["NORMAL"].as_u64().unwrap() as usize,
            3,
        );
        for (position, normal) in positions.iter().zip(normals) {
            if position == &[0., 0., 0.] {
                occurrences += 1;
                assert!((normal[1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
                assert!((normal[2] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
            }
        }
    }
    assert_eq!(occurrences, 2);
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
    assert!(String::from_utf8_lossy(&result.stderr).contains("skipped 1 degenerate triangles"));
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
