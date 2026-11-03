//! The binary against files built to hurt it, and the limits a host would run it under.
#![cfg(unix)]
use serde_json::json;
use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
static SEQ: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "hostile-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    /// Runs under a file-size limit in 512-byte blocks or larger, as the shell defines `ulimit -f`.
    /// SIGXFSZ is ignored, as a host has to: its default action kills the process, leaving the
    /// workspace and partial output behind. The ignored disposition carries across `exec`.
    fn import_limited(&self, blocks: u32, input: &str) -> Output {
        Command::new("sh")
            .arg("-c")
            .arg(format!(
                r#"trap '' XFSZ; ulimit -f {blocks}; exec "$0" import "$1" -o out.gltf"#
            ))
            .arg(env!("CARGO_BIN_EXE_objtools"))
            .arg(input)
            .current_dir(&self.0)
            .output()
            .unwrap()
    }
    #[cfg(unix)]
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.0)
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
/// A small file whose import writes far more: every face repeats the same vertices.
fn write_face_bomb(f: &Fixture) {
    let mut obj = String::from("v 0 0 0\nv 1 0 0\nv 0 1 0\n");
    obj.push_str(&"f 1 2 3\n".repeat(200_000));
    fs::write(f.0.join("in.obj"), obj).unwrap();
}
/// Many animation samplers over the same bytes, through their own buffer views: the JSON stays
/// small, and each view is copied to the output on its own.
fn write_view_bomb(f: &Fixture) {
    const VIEWS: usize = 64;
    let positions: Vec<u8> = [[0f32, 0., 0.], [1., 0., 0.], [0., 1., 0.]]
        .iter()
        .flatten()
        .flat_map(|c| c.to_le_bytes())
        .collect();
    fs::write(f.0.join("triangle.bin"), &positions).unwrap();
    fs::write(f.0.join("bomb.bin"), vec![0u8; 1 << 20]).unwrap();
    let mut views = vec![json!({"buffer": 0, "byteLength": positions.len()})];
    let mut accessors = vec![json!({
        "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
        "min": [0, 0, 0], "max": [1, 1, 0],
    })];
    let mut samplers = Vec::new();
    for i in 1..=VIEWS {
        samplers.push(json!({"input": i, "output": i}));
        views.push(json!({"buffer": 1, "byteLength": 1 << 20}));
        accessors.push(json!({
            "bufferView": i, "componentType": 5121, "count": 1, "type": "SCALAR",
        }));
    }
    let root = json!({
        "asset": {"version": "2.0"},
        "buffers": [
            {"uri": "triangle.bin", "byteLength": positions.len()},
            {"uri": "bomb.bin", "byteLength": 1 << 20},
        ],
        "bufferViews": views,
        "accessors": accessors,
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
        "nodes": [{"mesh": 0}, {}],
        "animations": [{
            "channels": [{"sampler": 0, "target": {"node": 1, "path": "translation"}}],
            "samplers": samplers,
        }],
        "scenes": [{"nodes": [0]}],
    });
    fs::write(f.0.join("in.gltf"), root.to_string()).unwrap();
}
fn assert_stopped_by_file_size_limit(f: &Fixture, out: &Output, inputs: &[&str]) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.signal(), None, "{stderr}");
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("File too large"), "{stderr}");
    assert_eq!(f.entries(), inputs);
}
#[test]
fn a_file_size_limit_stops_an_obj_import_and_leaves_nothing_behind() {
    let f = Fixture::new();
    write_face_bomb(&f);
    let out = f.import_limited(8, "in.obj");
    assert_stopped_by_file_size_limit(&f, &out, &["in.obj"]);
}
#[test]
fn a_file_size_limit_stops_a_view_bomb_and_leaves_nothing_behind() {
    let f = Fixture::new();
    write_view_bomb(&f);
    let out = f.import_limited(4096, "in.gltf");
    assert_stopped_by_file_size_limit(&f, &out, &["bomb.bin", "in.gltf", "triangle.bin"]);
}
#[test]
fn the_view_bomb_is_only_stopped_by_the_limit() {
    let f = Fixture::new();
    write_view_bomb(&f);
    let out = f.import_limited(1 << 20, "in.gltf");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(fs::metadata(f.0.join("out.bin")).unwrap().len() > 32 << 20);
}
#[test]
fn a_file_size_limit_stops_a_converted_texture_and_keeps_its_exit_code() {
    let f = Fixture::new();
    let pixels = image::RgbImage::from_fn(256, 256, |x, y| image::Rgb([x as u8, y as u8, 0]));
    pixels.save(f.0.join("t.bmp")).unwrap();
    fs::write(
        f.0.join("in.obj"),
        "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl m\nf 1/1 2/1 3/1\n",
    )
    .unwrap();
    fs::write(f.0.join("in.mtl"), "newmtl m\nmap_Kd t.bmp\n").unwrap();
    let out = f.import_limited(8, "in.obj");
    assert_stopped_by_file_size_limit(&f, &out, &["in.mtl", "in.obj", "t.bmp"]);
}
#[test]
fn a_directory_the_process_cannot_write_is_the_environment() {
    let f = Fixture::new();
    write_face_bomb(&f);
    let locked = f.0.join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    // Root ignores directory permissions, so there is nothing to test there.
    if fs::write(locked.join("probe"), "").is_ok() {
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_objtools"))
        .args(["import", "in.obj", "-o", "locked/out.gltf"])
        .current_dir(&f.0)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
}
