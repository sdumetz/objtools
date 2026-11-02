use serde_json::{json, Value};
use std::{
    f64::consts::PI,
    fmt::Write,
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
            "objtools-placeholder-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn file(&self, name: &str, body: &str) {
        fs::write(self.0.join(name), body).unwrap();
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn objtools(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_objtools"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Output {
        let out = self.objtools(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }
    /// Fails with the given text in stderr, leaving the directory as it found it.
    fn fails(&self, args: &[&str], message: &str) {
        let before = self.entries();
        let out = self.objtools(args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{args:?} succeeded");
        assert!(stderr.contains(message), "{args:?}: {stderr}");
        assert_eq!(self.entries(), before);
    }
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
    fn mesh(&self, name: &str) -> Mesh {
        let gltf = self.path(name);
        let root: Value = serde_json::from_slice(&fs::read(&gltf).unwrap()).unwrap();
        let bin = fs::read(gltf.with_extension("bin")).unwrap();
        assert_eq!(
            root["buffers"][0]["byteLength"].as_u64().unwrap(),
            bin.len() as u64
        );
        let primitive = &root["meshes"][0]["primitives"][0];
        let count = |id: &Value| {
            root["accessors"][id.as_u64().unwrap() as usize]["count"]
                .as_u64()
                .unwrap() as usize
        };
        let vertices = count(&primitive["attributes"]["POSITION"]);
        let f32_at = |o: usize| f32::from_le_bytes(bin[o..o + 4].try_into().unwrap());
        let read = |offset: usize| -> Vec<[f32; 3]> {
            (0..vertices)
                .map(|i| std::array::from_fn(|k| f32_at(i * 32 + offset + k * 4)))
                .collect()
        };
        let index_view = &root["bufferViews"][1];
        let start = index_view["byteOffset"].as_u64().unwrap() as usize;
        let accessor = &root["accessors"][primitive["indices"].as_u64().unwrap() as usize];
        let short = accessor["componentType"] == 5123;
        let indices = (0..accessor["count"].as_u64().unwrap() as usize)
            .map(|i| {
                if short {
                    u16::from_le_bytes(bin[start + i * 2..start + i * 2 + 2].try_into().unwrap())
                        as usize
                } else {
                    u32::from_le_bytes(bin[start + i * 4..start + i * 4 + 4].try_into().unwrap())
                        as usize
                }
            })
            .collect();
        let uvs = (0..vertices)
            .map(|i| std::array::from_fn(|k| f32_at(i * 32 + 24 + k * 4)))
            .collect();
        Mesh {
            positions: read(0),
            normals: read(12),
            uvs,
            indices,
            root,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Mesh {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<usize>,
    root: Value,
}
impl Mesh {
    fn faces(&self) -> usize {
        self.indices.len() / 3
    }
    fn bounds(&self) -> ([f32; 3], [f32; 3]) {
        let (mut min, mut max) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for p in &self.positions {
            for k in 0..3 {
                min[k] = min[k].min(p[k]);
                max[k] = max[k].max(p[k]);
            }
        }
        (min, max)
    }
    /// Signed volume by the divergence theorem: near the solid's for a closed, outward surface.
    fn volume(&self) -> f64 {
        self.indices
            .chunks(3)
            .map(|t| {
                let p = |i: usize| self.positions[t[i]].map(f64::from);
                let (a, b, c) = (p(0), p(1), p(2));
                (a[0] * (b[1] * c[2] - b[2] * c[1])
                    + a[1] * (b[2] * c[0] - b[0] * c[2])
                    + a[2] * (b[0] * c[1] - b[1] * c[0]))
                    / 6.
            })
            .sum()
    }
}
/// A quad grid over `f`, appended with OBJ indices continuing from `base`.
fn grid(obj: &mut String, base: &mut usize, n: usize, f: impl Fn(f64, f64) -> [f64; 3]) {
    for j in 0..=n {
        for i in 0..=n {
            let p = f(i as f64 / n as f64, j as f64 / n as f64);
            writeln!(obj, "v {} {} {}", p[0], p[1], p[2]).unwrap();
        }
    }
    let id = |i: usize, j: usize| *base + j * (n + 1) + i + 1;
    for j in 0..n {
        for i in 0..n {
            writeln!(obj, "f {} {} {}", id(i, j), id(i + 1, j), id(i + 1, j + 1)).unwrap();
            writeln!(obj, "f {} {} {}", id(i, j), id(i + 1, j + 1), id(i, j + 1)).unwrap();
        }
    }
    *base += (n + 1) * (n + 1);
}
/// An outward-facing axis-aligned cube, each side an n x n quad grid.
fn cube(obj: &mut String, base: &mut usize, n: usize, center: [f64; 3], size: f64) {
    let h = size / 2.;
    let [cx, cy, cz] = center;
    let at = |t: f64| (2. * t - 1.) * h;
    grid(obj, base, n, |u, v| [cx + h, cy + at(u), cz + at(v)]);
    grid(obj, base, n, |u, v| [cx - h, cy + at(v), cz + at(u)]);
    grid(obj, base, n, |u, v| [cx + at(v), cy + h, cz + at(u)]);
    grid(obj, base, n, |u, v| [cx + at(u), cy - h, cz + at(v)]);
    grid(obj, base, n, |u, v| [cx + at(u), cy + at(v), cz + h]);
    grid(obj, base, n, |u, v| [cx + at(v), cy + at(u), cz - h]);
}
fn cube_obj(n: usize) -> String {
    let mut obj = String::new();
    cube(&mut obj, &mut 0, n, [0.; 3], 10.);
    obj
}
fn torus_obj(n: usize) -> String {
    let mut obj = String::new();
    grid(&mut obj, &mut 0, n, |u, v| {
        let (a, b) = (u * 2. * PI, v * 2. * PI);
        let r = 10. + 3. * b.cos();
        [r * a.cos(), 3. * b.sin(), -r * a.sin()]
    });
    obj
}
fn assert_near(actual: f32, expected: f32, tolerance: f32) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} is not within {tolerance} of {expected}"
    );
}
#[test]
fn cube_placeholder_is_closed_outward_and_fits_its_source() {
    let f = Fixture::new();
    f.file("cube.obj", &cube_obj(8));
    f.ok(&["placeholder", "cube.obj"]);
    let placeholder = f.mesh("cube.placeholder.gltf");
    // Beveled edges remove a little volume, while vertices on the faces stay on them.
    let volume = placeholder.volume();
    assert!(volume > 950. && volume <= 1000.5, "volume {volume}");
    let (min, max) = placeholder.bounds();
    for k in 0..3 {
        assert_near(min[k], -5., 0.32);
        assert_near(max[k], 5., 0.32);
    }
    assert!(placeholder.normals.iter().all(|n| {
        let length = n.iter().map(|c| c * c).sum::<f32>().sqrt();
        (length - 1.).abs() < 1e-4
    }));
    // Each corner's outward normal agrees with its position on most of the surface.
    let outward = placeholder
        .positions
        .iter()
        .zip(&placeholder.normals)
        .filter(|(p, n)| p.iter().zip(*n).map(|(a, b)| a * b).sum::<f32>() > 0.)
        .count();
    assert_eq!(outward, placeholder.positions.len());
}
#[test]
fn default_resolution_is_16() {
    let f = Fixture::new();
    f.file("cube.obj", &cube_obj(1));
    f.ok(&["placeholder", "cube.obj"]);
    let mesh = f.mesh("cube.placeholder.gltf");
    assert_eq!(
        mesh.root["extras"]["objtools"]["placeholder"]["resolution"],
        16
    );
}
#[test]
fn every_face_is_box_projected_at_one_scale() {
    let f = Fixture::new();
    f.file("torus.obj", &torus_obj(64));
    f.ok(&["placeholder", "torus.obj"]);
    let mesh = f.mesh("torus.placeholder.gltf");
    let (min, max) = mesh.bounds();
    let longest = (0..3).map(|k| max[k] - min[k]).fold(0., f32::max);
    // The grid is a little longer than the placeholder, whose vertices sit inside its cells.
    let period = longest / 8.;
    let mut axes = [0; 3];
    for t in mesh.indices.chunks(3) {
        let p = |i: usize| mesh.positions[t[i]];
        let uv = |i: usize| mesh.uvs[t[i]];
        let edge = |i: usize| -> [f32; 3] { std::array::from_fn(|k| p(i)[k] - p(0)[k]) };
        let (a, b) = (edge(1), edge(2));
        let n = [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
        .map(f32::abs);
        let axis = (0..3).fold(0, |best, k| if n[k] > n[best] { k } else { best });
        axes[axis] += 1;
        for i in 1..3 {
            let expected = [
                (p(i)[(axis + 1) % 3] - p(0)[(axis + 1) % 3]) / period,
                (p(i)[(axis + 2) % 3] - p(0)[(axis + 2) % 3]) / period,
            ];
            for (k, e) in expected.into_iter().enumerate() {
                assert_near(uv(i)[k] - uv(0)[k], e, 0.08 * e.abs() + 0.001);
            }
        }
    }
    assert!(axes.iter().all(|n| *n > 0), "{axes:?}");
}
#[test]
fn checker_texture_lives_in_the_buffer() {
    let f = Fixture::new();
    f.file("cube.obj", &cube_obj(2));
    f.ok(&["placeholder", "cube.obj"]);
    let mesh = f.mesh("cube.placeholder.gltf");
    let root = &mesh.root;
    assert_eq!(
        root["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"]["index"],
        0
    );
    let view = &root["bufferViews"][root["images"][0]["bufferView"].as_u64().unwrap() as usize];
    let start = view["byteOffset"].as_u64().unwrap() as usize;
    let length = view["byteLength"].as_u64().unwrap() as usize;
    let bin = fs::read(f.path("cube.placeholder.bin")).unwrap();
    assert_eq!(start + length, bin.len());
    let image = image::load_from_memory(&bin[start..start + length])
        .unwrap()
        .to_rgb8();
    assert_eq!(image.dimensions(), (64, 64));
    let (blue, gray) = (
        image::Rgb([0x00, 0x89, 0xbf]),
        image::Rgb([0xd9, 0xd9, 0xd9]),
    );
    // Squares meet at hard edges, opposite corners match, and it repeats without a seam.
    assert_eq!(*image.get_pixel(0, 0), blue);
    assert_eq!(*image.get_pixel(31, 31), blue);
    assert_eq!(*image.get_pixel(32, 31), gray);
    assert_eq!(*image.get_pixel(31, 32), gray);
    assert_eq!(*image.get_pixel(63, 63), blue);
    assert_eq!(*image.get_pixel(63, 0), gray);
    assert!(root.get("extensionsUsed").is_none());
}
#[test]
fn output_size_follows_resolution_not_face_count() {
    let f = Fixture::new();
    f.file("coarse.obj", &cube_obj(1));
    f.file("dense.obj", &cube_obj(100));
    f.ok(&["placeholder", "coarse.obj", "--resolution", "32"]);
    f.ok(&["placeholder", "dense.obj", "--resolution", "32"]);
    f.ok(&[
        "placeholder",
        "dense.obj",
        "--resolution",
        "16",
        "-o",
        "dense16.gltf",
    ]);
    let (coarse, dense, low) = (
        f.mesh("coarse.placeholder.gltf"),
        f.mesh("dense.placeholder.gltf"),
        f.mesh("dense16.gltf"),
    );
    // Very different inputs reach the same surface, through the spanning and single-cell paths.
    assert_eq!(coarse.faces(), dense.faces());
    assert!((coarse.volume() - dense.volume()).abs() < 15.);
    assert!(dense.faces() > 3 * low.faces() && dense.faces() < 5 * low.faces());
    assert!(dense.faces() < 20_000);
    assert_eq!(
        dense.root["extras"]["objtools"]["placeholder"]["sourceFaces"],
        120_000
    );
    assert_eq!(
        low.root["extras"]["objtools"]["placeholder"]["resolution"],
        16
    );
}
#[test]
fn hidden_interior_geometry_adds_nothing() {
    let f = Fixture::new();
    let mut nested = String::new();
    let mut base = 0;
    cube(&mut nested, &mut base, 4, [0.; 3], 10.);
    cube(&mut nested, &mut base, 4, [0.; 3], 4.);
    f.file("nested.obj", &nested);
    f.file("plain.obj", &cube_obj(4));
    f.ok(&["placeholder", "nested.obj"]);
    f.ok(&["placeholder", "plain.obj"]);
    assert_eq!(
        f.mesh("nested.placeholder.gltf").faces(),
        f.mesh("plain.placeholder.gltf").faces()
    );
}
#[test]
fn holes_wider_than_a_few_cells_stay_open() {
    let f = Fixture::new();
    f.file("torus.obj", &torus_obj(64));
    f.ok(&["placeholder", "torus.obj"]);
    let placeholder = f.mesh("torus.placeholder.gltf");
    for p in &placeholder.positions {
        let radius = p[0].hypot(p[2]);
        assert!(radius > 6.5, "vertex {p:?} inside the hole");
        assert!(radius < 13.5, "vertex {p:?} outside the torus");
    }
    assert!(placeholder.volume() > 0.);
}
#[test]
fn open_surfaces_become_thin_sheets() {
    let f = Fixture::new();
    let mut obj = String::new();
    grid(&mut obj, &mut 0, 20, |u, v| [100. * u, 0., -100. * v]);
    f.file("plane.obj", &obj);
    f.ok(&["placeholder", "plane.obj"]);
    let placeholder = f.mesh("plane.placeholder.gltf");
    let (min, max) = placeholder.bounds();
    assert_near(max[1] - min[1], 0., 1e-3);
    assert_near(max[0] - min[0], 100., 8.);
    assert!(placeholder.volume().abs() < 1.);
}
#[test]
fn floaters_are_dropped_and_the_grid_refitted() {
    let f = Fixture::new();
    let mut obj = cube_obj(8);
    let mut base = 6 * 81;
    cube(&mut obj, &mut base, 1, [2000., 1500., -1800.], 0.1);
    f.file("noisy.obj", &obj);
    f.file("plain.obj", &cube_obj(8));
    let out = f.ok(&["placeholder", "noisy.obj"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("small disconnected"));
    f.ok(&["placeholder", "plain.obj"]);
    let (noisy, plain) = (
        f.mesh("noisy.placeholder.gltf"),
        f.mesh("plain.placeholder.gltf"),
    );
    let (min, max) = noisy.bounds();
    for k in 0..3 {
        assert_near(min[k], -5., 0.5);
        assert_near(max[k], 5., 0.5);
    }
    // Refitting recovers about the resolution of a model that never had floaters.
    let ratio = noisy.faces() as f64 / plain.faces() as f64;
    assert!(
        (0.8..1.2).contains(&ratio),
        "{} {}",
        noisy.faces(),
        plain.faces()
    );
}
#[test]
fn thread_counts_agree() {
    let f = Fixture::new();
    // Exact only because the sums are fixed-point, which is an implementation detail rather than
    // a promise to users: floating-point sums would need this to compare within a tolerance.
    // Enough faces that the requested threads are actually used.
    f.file("torus.obj", &torus_obj(320));
    f.ok(&["import", "torus.obj", "-o", "torus.gltf"]);
    f.ok(&[
        "placeholder",
        "torus.gltf",
        "--threads",
        "1",
        "-o",
        "one.gltf",
    ]);
    for threads in ["2", "7"] {
        let output = format!("{threads}.gltf");
        f.ok(&[
            "placeholder",
            "torus.gltf",
            "--threads",
            threads,
            "-o",
            &output,
        ]);
        assert!(
            fs::read(f.path("one.bin")).unwrap()
                == fs::read(f.path(&format!("{threads}.bin"))).unwrap()
        );
    }
}
#[test]
fn obj_and_its_imported_gltf_give_the_same_placeholder() {
    let f = Fixture::new();
    f.file("torus.obj", &torus_obj(32));
    f.ok(&["import", "torus.obj", "--center", "-o", "torus.gltf"]);
    f.ok(&["placeholder", "torus.gltf", "-o", "from-gltf.gltf"]);
    f.ok(&[
        "placeholder",
        "torus.obj",
        "--center",
        "-o",
        "from-obj.gltf",
    ]);
    assert!(
        fs::read(f.path("from-gltf.bin")).unwrap() == fs::read(f.path("from-obj.bin")).unwrap()
    );
}
#[test]
fn placeholder_shares_its_source_frame_and_metadata() {
    let f = Fixture::new();
    let mut obj = String::new();
    cube(&mut obj, &mut 0, 4, [600_000., 6_400_000., 300.], 10.);
    f.file("geo.obj", &obj);
    f.ok(&["placeholder", "geo.obj", "--center", "--up-axis", "z"]);
    let placeholder = f.mesh("geo.placeholder.gltf");
    let node = &placeholder.root["nodes"][0];
    assert_eq!(node["rotation"].as_array().unwrap().len(), 4);
    let extras = &placeholder.root["extras"]["objtools"];
    assert_eq!(extras["origin"][0], "600000");
    assert_eq!(extras["origin"][1], "6400000");
    assert_eq!(extras["centered"], true);
    assert_eq!(extras["sourceUpAxis"], "z");
    let (min, max) = placeholder.bounds();
    assert_near(min[0], -5., 0.5);
    assert_near(max[2], 5., 0.5);
    // Without a Z-up source, no rotation.
    f.ok(&["placeholder", "geo.obj", "--center", "-o", "y.gltf"]);
    assert!(f.mesh("y.gltf").root["nodes"][0].get("rotation").is_none());
}
#[test]
fn uncentered_georeferenced_input_warns_about_float32() {
    let f = Fixture::new();
    let mut obj = String::new();
    cube(&mut obj, &mut 0, 4, [600_000., 6_400_000., 300.], 1.);
    f.file("geo.obj", &obj);
    let out = f.objtools(&["placeholder", "geo.obj"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("--center"));
    let out = f.ok(&["placeholder", "geo.obj", "--center", "-o", "centered.gltf"]);
    assert!(!String::from_utf8_lossy(&out.stderr).contains("lost detail"));
}
#[test]
fn placeholder_is_a_model_inspect_can_read() {
    let f = Fixture::new();
    f.file("cube.obj", &cube_obj(2));
    f.ok(&["placeholder", "cube.obj"]);
    let out = f.ok(&["inspect", "--json", "cube.placeholder.gltf"]);
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        report["faces"],
        f.mesh("cube.placeholder.gltf").faces() as u64
    );
}
/// A glTF of a 2x2x2 cube around the origin, as one mesh, with these nodes and scenes, and whatever
/// else `root` adds. `bin` follows the cube's 240 bytes, for further accessors.
fn cube_gltf(f: &Fixture, name: &str, mut root: Value, bin: &[u8]) {
    let mut data = Vec::new();
    for corner in 0..8 {
        for k in 0..3 {
            let v: f32 = if corner >> k & 1 == 0 { -1. } else { 1. };
            data.extend_from_slice(&v.to_le_bytes());
        }
    }
    let faces: [[u16; 3]; 12] = [
        [0, 2, 1],
        [1, 2, 3],
        [4, 5, 6],
        [5, 7, 6],
        [0, 1, 4],
        [1, 5, 4],
        [2, 6, 3],
        [3, 6, 7],
        [0, 4, 2],
        [2, 4, 6],
        [1, 3, 5],
        [3, 7, 5],
    ];
    for i in faces.iter().flatten() {
        data.extend_from_slice(&i.to_le_bytes());
    }
    assert_eq!(data.len(), 168);
    data.resize(240, 0);
    data.extend_from_slice(bin);
    fs::write(f.path(&format!("{name}.bin")), &data).unwrap();
    let cube = json!({
        "asset": {"version": "2.0"},
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "indices": 1}]}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 8, "type": "VEC3",
             "min": [-1, -1, -1], "max": [1, 1, 1]},
            {"bufferView": 1, "componentType": 5123, "count": 36, "type": "SCALAR"},
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 96},
            {"buffer": 0, "byteOffset": 96, "byteLength": 72},
        ],
        "buffers": [{"uri": format!("{name}.bin"), "byteLength": data.len()}],
    });
    let extend = |key: &str, root: &mut Value| {
        let extra = root[key].take();
        let mut list = cube[key].as_array().unwrap().clone();
        list.extend(extra.as_array().cloned().unwrap_or_default());
        root[key] = Value::Array(list);
    };
    for key in ["meshes", "accessors", "bufferViews"] {
        extend(key, &mut root);
    }
    for key in ["asset", "buffers"] {
        root[key] = cube[key].clone();
    }
    f.file(&format!("{name}.gltf"), &root.to_string());
}
/// Imports `root` and builds its placeholder.
fn placeholder_of(f: &Fixture, root: Value, bin: &[u8]) -> Mesh {
    cube_gltf(f, "source", root, bin);
    f.ok(&["import", "source.gltf", "-o", "ours.gltf"]);
    f.ok(&["placeholder", "ours.gltf", "-o", "out.gltf"]);
    f.mesh("out.gltf")
}
/// A vertex through the nodes from `id` up to the root.
fn to_world(root: &Value, id: usize, mut p: [f64; 3]) -> [f64; 3] {
    let numbers = |v: &Value, default: Vec<f64>| -> Vec<f64> {
        v.as_array()
            .map_or(default, |a| a.iter().map(|n| n.as_f64().unwrap()).collect())
    };
    let nodes = root["nodes"].as_array().unwrap();
    let mut next = Some(id);
    while let Some(id) = next {
        let node = &nodes[id];
        if node.get("matrix").is_some() {
            let m = numbers(&node["matrix"], vec![]);
            p = std::array::from_fn(|i| {
                m[i] * p[0] + m[4 + i] * p[1] + m[8 + i] * p[2] + m[12 + i]
            });
        } else {
            let s = numbers(&node["scale"], vec![1.; 3]);
            let q = numbers(&node["rotation"], vec![0., 0., 0., 1.]);
            let t = numbers(&node["translation"], vec![0.; 3]);
            let v = [p[0] * s[0], p[1] * s[1], p[2] * s[2]];
            // v + 2w(u x v) + 2u x (u x v), for the quaternion's vector part u.
            let cross = |a: [f64; 3], b: [f64; 3]| {
                [
                    a[1] * b[2] - a[2] * b[1],
                    a[2] * b[0] - a[0] * b[2],
                    a[0] * b[1] - a[1] * b[0],
                ]
            };
            let c = cross([q[0], q[1], q[2]], v);
            let d = cross([q[0], q[1], q[2]], c);
            p = std::array::from_fn(|k| v[k] + 2. * q[3] * c[k] + 2. * d[k] + t[k]);
        }
        next = nodes.iter().position(|n| {
            n["children"]
                .as_array()
                .is_some_and(|c| c.contains(&json!(id)))
        });
    }
    p
}
/// The placeholder's vertices where a renderer shows them.
fn world_vertices(mesh: &Mesh) -> Vec<[f64; 3]> {
    let nodes = mesh.root["nodes"].as_array().unwrap();
    let id = nodes.iter().position(|n| n.get("mesh").is_some()).unwrap();
    mesh.positions
        .iter()
        .map(|p| to_world(&mesh.root, id, p.map(f64::from)))
        .collect()
}
/// Checks the vertices' bounds, within `tolerance`.
fn assert_world_bounds(mesh: &Mesh, min: [f64; 3], max: [f64; 3], tolerance: f64) {
    let vertices = world_vertices(mesh);
    for k in 0..3 {
        let lo = vertices.iter().map(|v| v[k]).fold(f64::INFINITY, f64::min);
        let hi = vertices
            .iter()
            .map(|v| v[k])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (lo - min[k]).abs() <= tolerance,
            "min {k}: {lo} vs {}",
            min[k]
        );
        assert!(
            (hi - max[k]).abs() <= tolerance,
            "max {k}: {hi} vs {}",
            max[k]
        );
    }
}
#[test]
fn a_shared_transform_is_copied_as_written_and_positions_stay_stored() {
    let f = Fixture::new();
    let s = std::f64::consts::FRAC_1_SQRT_2;
    let node = json!({"mesh": 0, "translation": [100, 200, 300], "rotation": [s, 0, 0, s],
                      "scale": [2, 2, 8]});
    let mesh = placeholder_of(
        &f,
        json!({"scene": 0, "scenes": [{"nodes": [0]}], "nodes": [node]}),
        &[],
    );
    let nodes = mesh.root["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1);
    for key in ["translation", "rotation", "scale"] {
        assert_eq!(nodes[0][key], node[key], "{key}");
    }
    let (min, max) = mesh.bounds();
    for k in 0..3 {
        assert_near(min[k], -1., 0.15);
        assert_near(max[k], 1., 0.15);
    }
    // The 90 degree turn around X sends the scaled Z axis to -Y, and Y to Z.
    assert_world_bounds(&mesh, [98., 192., 298.], [102., 208., 302.], 1.2);
}
#[test]
fn parents_carry_over_and_empty_nodes_do_not() {
    let f = Fixture::new();
    let mut matrix = vec![1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0.];
    matrix.extend([10., 20., 30., 1.]);
    let s = std::f64::consts::FRAC_1_SQRT_2;
    // The child's rotation under this scale shears the mesh: no single node could express it.
    let mesh = placeholder_of(
        &f,
        json!({"scene": 0, "scenes": [{"nodes": [0]}], "nodes": [
            {"matrix": matrix, "children": [1]},
            {"scale": [4, 1, 1], "children": [2]},
            {"children": [3]},
            {"mesh": 0, "rotation": [0, 0, 0.3826834323650898, 0.9238795325112867],
             "translation": [0, s, 0]},
        ]}),
        &[],
    );
    let nodes = mesh.root["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 3);
    assert_eq!(nodes[0]["matrix"].as_array().unwrap().len(), 16);
    assert_eq!(nodes[0]["children"], json!([1]));
    assert_eq!(nodes[1]["scale"], json!([4, 1, 1]));
    assert_eq!(nodes[1]["children"], json!([2]));
    assert_eq!(nodes[2]["mesh"], json!(0));
    assert_eq!(nodes[2]["translation"], json!([0, s, 0]));
    assert_eq!(mesh.root["scenes"][0]["nodes"], json!([0]));
    let source: Value = serde_json::from_slice(&fs::read(f.path("source.gltf")).unwrap()).unwrap();
    let corners = (0..8).map(|c| {
        let p = std::array::from_fn(|k| if c >> k & 1 == 0 { -1. } else { 1. });
        to_world(&source, 3, p)
    });
    let (mut min, mut max) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for c in corners {
        for k in 0..3 {
            min[k] = min[k].min(c[k]);
            max[k] = max[k].max(c[k]);
        }
    }
    assert_world_bounds(&mesh, min, max, 1.5);
}
#[test]
fn different_transforms_are_placed_in_the_first_ones_frame() {
    let f = Fixture::new();
    let mesh = placeholder_of(
        &f,
        json!({"scene": 0, "scenes": [{"nodes": [0, 1]}], "nodes": [
            {"mesh": 0, "translation": [10, 0, 0]},
            {"mesh": 0, "translation": [20, 0, 0], "scale": [2, 2, 2]},
        ]}),
        &[],
    );
    let nodes = mesh.root["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["translation"], json!([10, 0, 0]));
    assert_world_bounds(&mesh, [9., -2., -2.], [22., 2., 2.], 0.5);
    // Two cubes, with the gap between them left empty.
    let vertices = world_vertices(&mesh);
    assert!(vertices.iter().any(|v| v[0] < 11.5));
    assert!(vertices.iter().any(|v| v[0] > 17.5));
    assert!(!vertices.iter().any(|v| v[0] > 12.5 && v[0] < 16.5));
    let faces = &mesh.root["extras"]["objtools"]["placeholder"]["sourceFaces"];
    assert_eq!(faces, 24, "a reused mesh counts once per instance");
}
#[test]
fn only_the_default_scene_counts() {
    let f = Fixture::new();
    let mesh = placeholder_of(
        &f,
        json!({"scene": 1, "scenes": [{"nodes": [0]}, {"nodes": [1]}], "nodes": [
            {"mesh": 0, "translation": [1000, 0, 0]},
            {"mesh": 0, "translation": [5, 0, 0]},
            {"mesh": 0, "translation": [-1000, 0, 0]},
        ]}),
        &[],
    );
    assert_eq!(mesh.root["nodes"][0]["translation"], json!([5, 0, 0]));
    assert_world_bounds(&mesh, [4., -1., -1.], [6., 1., 1.], 0.15);
}
#[test]
fn a_scene_without_meshes_is_an_error() {
    let f = Fixture::new();
    cube_gltf(
        &f,
        "source",
        json!({"scene": 0, "scenes": [{"nodes": [0]}], "nodes": [{"name": "empty"}]}),
        &[],
    );
    f.ok(&["import", "source.gltf", "-o", "ours.gltf"]);
    f.fails(
        &["placeholder", "ours.gltf", "-o", "out.gltf"],
        "shows no mesh",
    );
}
#[test]
fn gpu_instances_are_all_placed() {
    let f = Fixture::new();
    let mut bin = Vec::new();
    for v in [0f32, 0., 0., 8., 0., 0.] {
        bin.extend_from_slice(&v.to_le_bytes());
    }
    let mesh = placeholder_of(
        &f,
        json!({
            "scene": 0, "scenes": [{"nodes": [0]}],
            "extensionsUsed": ["EXT_mesh_gpu_instancing"],
            "extensionsRequired": ["EXT_mesh_gpu_instancing"],
            "accessors": [{"bufferView": 2, "componentType": 5126, "count": 2, "type": "VEC3"}],
            "bufferViews": [{"buffer": 0, "byteOffset": 240, "byteLength": 24}],
            "nodes": [{"mesh": 0, "translation": [100, 0, 0], "extensions": {
                "EXT_mesh_gpu_instancing": {"attributes": {"TRANSLATION": 2}},
            }}],
        }),
        &bin,
    );
    assert_eq!(mesh.root["nodes"][0]["translation"], json!([100, 0, 0]));
    assert!(mesh.root["nodes"][0].get("extensions").is_none());
    assert_world_bounds(&mesh, [99., -1., -1.], [109., 1., 1.], 0.5);
    let vertices = world_vertices(&mesh);
    assert!(!vertices.iter().any(|v| v[0] > 102. && v[0] < 106.));
}
#[test]
fn a_flattening_transform_cannot_be_the_frame() {
    let f = Fixture::new();
    let mesh = placeholder_of(
        &f,
        json!({"scene": 0, "scenes": [{"nodes": [0, 1]}], "nodes": [
            {"mesh": 0, "scale": [1, 1, 0], "translation": [0, 0, 7]},
            {"mesh": 0, "translation": [0, 0, 7]},
        ]}),
        &[],
    );
    assert_eq!(mesh.root["nodes"][0]["translation"], json!([0, 0, 7]));
    assert!(mesh.root["nodes"][0].get("scale").is_none());
    assert_world_bounds(&mesh, [-1., -1., 6.], [1., 1., 8.], 0.15);
}
#[test]
fn a_flat_model_is_built_in_world_coordinates() {
    let f = Fixture::new();
    let mesh = placeholder_of(
        &f,
        json!({"scene": 0, "scenes": [{"nodes": [0]}], "nodes": [
            {"mesh": 0, "scale": [1, 1, 0], "translation": [3, 0, 7]},
        ]}),
        &[],
    );
    assert!(mesh.root["nodes"][0].get("scale").is_none());
    assert!(mesh.root["nodes"][0].get("translation").is_none());
    assert_world_bounds(&mesh, [2., -1., 7.], [4., 1., 7.], 0.2);
}
#[test]
fn a_skinned_node_stays_where_its_mesh_is_stored() {
    let f = Fixture::new();
    let mut bin = Vec::new();
    for v in [
        1f32, 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
    ] {
        bin.extend_from_slice(&v.to_le_bytes());
    }
    let mesh = placeholder_of(
        &f,
        json!({
            "scene": 0, "scenes": [{"nodes": [0, 1]}],
            "accessors": [{"bufferView": 2, "componentType": 5126, "count": 1, "type": "MAT4"}],
            "bufferViews": [{"buffer": 0, "byteOffset": 240, "byteLength": 64}],
            "skins": [{"joints": [1], "inverseBindMatrices": 2}],
            "nodes": [
                {"mesh": 0, "skin": 0, "translation": [500, 0, 0]},
                {"name": "joint"},
            ],
        }),
        &bin,
    );
    assert!(mesh.root["nodes"][0].get("translation").is_none());
    assert_world_bounds(&mesh, [-1.; 3], [1.; 3], 0.15);
}
#[test]
fn foreign_gltf_is_rejected() {
    let f = Fixture::new();
    f.file("cube.obj", &cube_obj(1));
    f.ok(&["import", "cube.obj", "-o", "ours.gltf"]);
    let mut root: Value = serde_json::from_slice(&fs::read(f.path("ours.gltf")).unwrap()).unwrap();
    root.as_object_mut().unwrap().remove("extras");
    f.file("foreign.gltf", &root.to_string());
    f.fails(&["placeholder", "foreign.gltf"], "objtools import");
}
#[test]
fn bad_arguments_and_existing_outputs_leave_nothing_behind() {
    let f = Fixture::new();
    f.file("cube.obj", &cube_obj(1));
    f.ok(&["import", "cube.obj", "-o", "cube.gltf"]);
    f.fails(
        &["placeholder", "cube.gltf", "--center"],
        "only apply to inputs",
    );
    f.fails(
        &["placeholder", "cube.gltf", "--up-axis", "z"],
        "only apply to inputs",
    );
    f.fails(
        &["placeholder", "cube.obj", "--resolution", "1"],
        "--resolution",
    );
    f.fails(
        &["placeholder", "cube.obj", "--resolution", "129"],
        "--resolution",
    );
    f.fails(&["placeholder", "cube.obj", "--threads", "0"], "--threads");
    f.fails(&["placeholder", "missing.obj"], "no such file");
    f.fails(
        &["placeholder", "cube.obj", "-o", "out.obj"],
        ".gltf extension",
    );
    f.fails(
        &["placeholder", "cube.obj", "-o", "cube.gltf"],
        "already exists",
    );
    // Zero-area faces leave import nothing to write.
    f.file("line.obj", "v 0 0 0\nv 1 0 0\nv 2 0 0\nf 1 2 3\n");
    f.fails(
        &["placeholder", "line.obj"],
        "no supported triangle geometry",
    );
}
#[test]
fn workspace_defaults_to_output_folder_and_is_removed() {
    let f = Fixture::new();
    f.file("cube.obj", &cube_obj(1));
    f.ok(&["placeholder", "cube.obj"]);
    assert_eq!(
        f.entries(),
        ["cube.obj", "cube.placeholder.bin", "cube.placeholder.gltf"]
    );
}
