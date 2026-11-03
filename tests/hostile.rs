//! The binary against files built to hurt it, and the limits a host would run it under.
use serde_json::{json, Value};
#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt, process::ExitStatusExt};
use std::{
    cell::Cell,
    fs,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
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
    #[cfg(unix)]
    fn import_limited(&self, blocks: u32, input: &str) -> Output {
        self.limited(blocks, &["import", input, "-o", "out.gltf"])
    }
    #[cfg(unix)]
    /// Runs under a file-size limit in 512-byte blocks or larger, as the shell defines `ulimit -f`.
    /// SIGXFSZ is ignored, as a host has to: its default action kills the process, leaving the
    /// workspace and partial output behind. The ignored disposition carries across `exec`.
    fn limited(&self, blocks: u32, args: &[&str]) -> Output {
        Command::new("sh")
            .arg("-c")
            .arg(format!(
                r#"trap '' XFSZ; ulimit -f {blocks}; exec "$0" "$@""#
            ))
            .arg(env!("CARGO_BIN_EXE_objtools"))
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap()
    }
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
fn write_face_bomb(f: &Fixture, faces: usize) {
    let mut obj = String::from("v 0 0 0\nv 1 0 0\nv 0 1 0\n");
    obj.push_str(&"f 1 2 3\n".repeat(faces));
    fs::write(f.0.join("in.obj"), obj).unwrap();
}
/// Many animation samplers over the same bytes, through their own buffer views: the JSON stays
/// small, and each view is copied to the output on its own.
#[cfg(unix)]
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
#[cfg(unix)]
fn assert_stopped_by_file_size_limit(f: &Fixture, out: &Output, inputs: &[&str]) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_harmless(&stderr);
    assert_eq!(out.status.signal(), None, "{stderr}");
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("File too large"), "{stderr}");
    assert_eq!(f.entries(), inputs);
}
#[cfg(unix)]
#[test]
fn a_file_size_limit_stops_an_obj_import_and_leaves_nothing_behind() {
    let f = Fixture::new();
    write_face_bomb(&f, 200_000);
    let out = f.import_limited(8, "in.obj");
    assert_stopped_by_file_size_limit(&f, &out, &["in.obj"]);
}
#[cfg(unix)]
#[test]
fn a_file_size_limit_stops_a_view_bomb_and_leaves_nothing_behind() {
    let f = Fixture::new();
    write_view_bomb(&f);
    let out = f.import_limited(4096, "in.gltf");
    assert_stopped_by_file_size_limit(&f, &out, &["bomb.bin", "in.gltf", "triangle.bin"]);
}
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
#[test]
fn a_file_size_limit_stops_inspect_marking_an_enormous_texture() {
    // A header is all it takes: the map of which texels are used is sized from what it declares.
    let f = Fixture::new();
    let mut bmp = b"BM".to_vec();
    bmp.extend_from_slice(&[0; 8]);
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&60_000u32.to_le_bytes());
    bmp.extend_from_slice(&60_000u32.to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&24u16.to_le_bytes());
    bmp.extend_from_slice(&[0; 24]);
    fs::write(f.0.join("t.bmp"), bmp).unwrap();
    let mut buffer = positions();
    buffer.extend([0u8; 24]);
    fs::write(f.0.join("m.bin"), buffer).unwrap();
    fs::write(f.0.join("in.gltf"), gltf_with_image("t.bmp")).unwrap();
    let out = f.limited(8, &["inspect", "in.gltf"]);
    assert_stopped_by_file_size_limit(&f, &out, &["in.gltf", "m.bin", "t.bmp"]);
}
#[test]
fn a_job_killed_midway_does_not_block_the_next_one() {
    let f = Fixture::new();
    write_face_bomb(&f, 4_000_000);
    let mut job = Command::new(env!("CARGO_BIN_EXE_objtools"))
        .args(["import", "in.obj", "-o", "killed.gltf"])
        .current_dir(&f.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let workspaces = || {
        let entries = f.entries();
        entries
            .into_iter()
            .filter(|e| e.starts_with("objtools-"))
            .collect::<Vec<_>>()
    };
    let start = Instant::now();
    while workspaces().is_empty() {
        assert!(
            job.try_wait().unwrap().is_none(),
            "finished before it could be killed"
        );
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "never made a workspace"
        );
        thread::sleep(Duration::from_millis(1));
    }
    job.kill().unwrap();
    job.wait().unwrap();
    let left = workspaces();
    assert_eq!(left.len(), 1);
    fs::write(f.0.join("next.obj"), TRIANGLE).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_objtools"))
        .args(["import", "next.obj", "-o", "next.gltf"])
        .current_dir(&f.0)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    // Removing what a killed job leaves is for whoever owns the directory.
    assert_eq!(workspaces(), left);
}
#[cfg(unix)]
#[test]
fn a_leftover_with_this_process_id_does_not_block_the_job() {
    // Process IDs repeat in every container, so a killed job's leftover can have the next one's.
    // `exec` keeps the shell's ID, which is how the test knows it in advance.
    let f = Fixture::new();
    fs::write(f.0.join("in.obj"), TRIANGLE).unwrap();
    let out = Command::new("sh")
        .arg("-c")
        .arg(r#"mkdir "objtools-$$"; exec "$0" import in.obj -o out.gltf"#)
        .arg(env!("CARGO_BIN_EXE_objtools"))
        .current_dir(&f.0)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
}
#[cfg(unix)]
#[test]
fn a_directory_the_process_cannot_write_is_the_environment() {
    let f = Fixture::new();
    write_face_bomb(&f, 200_000);
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
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(4), "{stderr}");
}

/// A model in `job`, and in `outside` the files a hostile one would like to reach. The flag says
/// a file in the model names a real path of this machine, which a message may then repeat.
struct Upload(Fixture, Cell<bool>);
const CANARY: &[u8] = b"objtools-canary";
const REFUSED: &str = "not found, or outside the model's folder";
impl Upload {
    fn new() -> Self {
        let f = Fixture::new();
        fs::create_dir(f.0.join("job")).unwrap();
        fs::create_dir(f.0.join("outside")).unwrap();
        let outside = f.0.join("outside");
        fs::write(outside.join("secret.bin"), positions_with_canary()).unwrap();
        fs::write(outside.join("secret.mtl"), "newmtl leaked\nKd 1 0 0\n").unwrap();
        image::RgbImage::new(4, 4)
            .save(outside.join("secret.png"))
            .unwrap();
        Self(f, Cell::new(false))
    }
    fn names_a_real_path(&self) {
        self.1.set(true);
    }
    fn job(&self) -> PathBuf {
        self.0 .0.join("job")
    }
    fn write(&self, name: &str, body: &str) {
        fs::write(self.job().join(name), body).unwrap();
    }
    /// Never waits for more than a minute, since what's being tested may be a hang. Whatever the
    /// test goes on to check, every run has to hold to `assert_nothing_escaped`.
    #[track_caller]
    fn run(&self, args: &[&str]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_objtools"))
            .args(args)
            .current_dir(self.job())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while child.try_wait().unwrap().is_none() {
            if start.elapsed() > Duration::from_secs(60) {
                child.kill().unwrap();
                panic!("objtools hung on {args:?}");
            }
            thread::sleep(Duration::from_millis(5));
        }
        let out = child.wait_with_output().unwrap();
        self.assert_nothing_escaped(args, &out);
        out
    }
    /// What no file may cause, whether it is accepted or rejected.
    #[track_caller]
    fn assert_nothing_escaped(&self, args: &[&str], out: &Output) {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Not a panic, and not killed by a signal.
        assert!(
            matches!(out.status.code(), Some(0 | 1 | 3 | 4)),
            "{args:?}: {:?}: {stderr}",
            out.status
        );
        assert_harmless(&stderr);
        assert!(
            stderr.lines().count() <= 20 && stderr.len() < 4_000,
            "{args:?}: {} bytes: {stderr}",
            stderr.len()
        );
        // The report goes where a person reads it too. JSON escapes what it has to on its own.
        if args.first() == Some(&"inspect") && !args.contains(&"--json") {
            assert_harmless(&String::from_utf8_lossy(&out.stdout));
        }
        for internal in ["objtools-", " Os {"] {
            assert!(!stderr.contains(internal), "{args:?}: {stderr}");
        }
        if !self.1.get() {
            let temp = std::env::temp_dir();
            let canonical = fs::canonicalize(&temp).unwrap_or_else(|_| temp.clone());
            for path in [temp, canonical] {
                let path = path.to_string_lossy();
                assert!(
                    !stderr.contains(path.trim_end_matches('/')),
                    "{args:?}: {stderr}"
                );
            }
            assert!(!stderr.contains("hostile-"), "{args:?}: {stderr}");
        }
        let names = |dir: PathBuf| -> Vec<String> {
            let entries = fs::read_dir(dir).unwrap();
            let names = entries.map(|e| e.unwrap().file_name().to_string_lossy().into_owned());
            let mut names: Vec<String> = names.collect();
            names.sort();
            names
        };
        let outside = names(self.0 .0.join("outside"));
        assert_eq!(
            outside,
            ["secret.bin", "secret.mtl", "secret.png"],
            "{args:?}"
        );
        for name in names(self.job()).iter().chain(&names(self.0 .0.clone())) {
            assert!(!name.starts_with("objtools-"), "{args:?}: {name}");
        }
    }
    fn stderr_of(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }
    /// Refused by both commands, with nothing of it in anything written.
    fn assert_refused(&self, input: &str) {
        for args in [
            ["import", input, "-o", "out.gltf"].as_slice(),
            &["inspect", input],
        ] {
            let stderr = self.stderr_of(args);
            assert!(stderr.contains(REFUSED), "{args:?}: {stderr}");
        }
        for entry in fs::read_dir(self.job()).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            assert!(!name.starts_with("out"), "{name}");
            // Links are skipped because an allowed read wouldn't follow them, and a FIFO would hang.
            if !fs::symlink_metadata(&path).unwrap().is_file() {
                continue;
            }
            let bytes = fs::read(&path).unwrap();
            assert!(!bytes.windows(CANARY.len()).any(|w| w == CANARY), "{name}");
        }
    }
    /// Followed by both commands, in each way of placing images, with nothing published that
    /// leads out of the output's directory.
    #[track_caller]
    fn assert_followed_and_contained(&self, input: &str, case: &str) {
        let out = self.run(&["inspect", input, "--json"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{case}: {stderr}");
        let report: Value = serde_json::from_slice(&out.stdout).unwrap();
        if let Some(texture) = report["textures"].as_array().and_then(|t| t.first()) {
            assert_eq!(texture["width"], 8, "{case}: {report}");
        }
        fs::create_dir_all(self.job().join("elsewhere")).unwrap();
        for (output, extra) in [
            ("auto.gltf", [].as_slice()),
            ("keep.gltf", ["--images", "keep"].as_slice()),
            ("copy.gltf", ["--images", "copy"].as_slice()),
            ("elsewhere/auto.gltf", [].as_slice()),
        ] {
            let mut args = vec!["import", input, "-o", output];
            args.extend_from_slice(extra);
            let out = self.run(&args);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(out.status.code(), Some(0), "{case}: {args:?}: {stderr}");
            assert_self_contained(&self.job().join(output));
        }
    }
    fn assert_followed_when_trusted(&self, input: &str) {
        let out = self.run(&["import", input, "-o", "out.gltf", "--trusted"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{stderr}");
        let out = self.run(&["inspect", input, "--trusted"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{stderr}");
    }
}
fn positions() -> Vec<u8> {
    [[0f32, 0., 0.], [1., 0., 0.], [0., 1., 0.]]
        .iter()
        .flatten()
        .flat_map(|c| c.to_le_bytes())
        .collect()
}
fn positions_with_canary() -> Vec<u8> {
    let mut bytes = positions();
    bytes.extend_from_slice(CANARY);
    bytes
}
fn uv_buffer(u: &Upload) {
    let uvs: Vec<u8> = [[0f32, 0.], [1., 0.], [0., 1.]]
        .iter()
        .flatten()
        .flat_map(|c| c.to_le_bytes())
        .collect();
    let mut bytes = positions();
    bytes.extend(uvs);
    fs::write(u.job().join("m.bin"), bytes).unwrap();
}
fn gltf_with_buffer(uri: &str) -> String {
    json!({
        "asset": {"version": "2.0"},
        "buffers": [{"uri": uri, "byteLength": 36}],
        "bufferViews": [{"buffer": 0, "byteLength": 36}],
        "accessors": [{
            "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
            "min": [0, 0, 0], "max": [1, 1, 0],
        }],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
        "nodes": [{"mesh": 0}],
        "scenes": [{"nodes": [0]}],
    })
    .to_string()
}
fn gltf_with_image(uri: &str) -> String {
    let mut root: Value = serde_json::from_str(&gltf_with_buffer("m.bin")).unwrap();
    root["buffers"][0]["byteLength"] = json!(60);
    root["bufferViews"] = json!([
        {"buffer": 0, "byteLength": 36},
        {"buffer": 0, "byteOffset": 36, "byteLength": 24},
    ]);
    root["accessors"]
        .as_array_mut()
        .unwrap()
        .push(json!({"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC2"}));
    root["meshes"][0]["primitives"][0]["attributes"]["TEXCOORD_0"] = json!(1);
    root["meshes"][0]["primitives"][0]["material"] = json!(0);
    root["materials"] = json!([{"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}]);
    root["textures"] = json!([{"source": 0}]);
    root["images"] = json!([{"uri": uri}]);
    root.to_string()
}
const TEXTURED_OBJ: &str =
    "mtllib in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 0 1\nusemtl m\nf 1/1 2/2 3/3\n";
fn vrml_with_url(url: &str) -> String {
    // A VRML string escapes with a backslash, so a literal one is written twice.
    let url = url.replace('\\', "\\\\");
    format!(
        "#VRML V2.0 utf8\nShape {{ appearance Appearance {{ texture ImageTexture {{ url \"{url}\" }} }}\n\
         geometry IndexedFaceSet {{ coord Coordinate {{ point [0 0 0, 1 0 0, 0 1 0] }} coordIndex [0 1 2 -1]\n\
         texCoord TextureCoordinate {{ point [0 0, 1 0, 0 1] }} texCoordIndex [0 1 2 -1] }} }}\n"
    )
}
/// A GLB holding `root` as its JSON and no binary chunk, so its buffers are all external.
fn glb(root: &str) -> Vec<u8> {
    let mut json = root.as_bytes().to_vec();
    json.resize(json.len().next_multiple_of(4), b' ');
    let length = 12 + 8 + json.len() as u32;
    let mut bytes = b"glTF".to_vec();
    for field in [2, length, json.len() as u32, 0x4E4F_534A] {
        bytes.extend_from_slice(&u32::to_le_bytes(field));
    }
    bytes.extend(json);
    bytes
}
/// Writes `in.*` referring to `reference` as one of the kinds of reference, and returns its name.
fn write_reference(u: &Upload, kind: Kind, reference: &str) -> &'static str {
    match kind {
        Kind::GlbBuffer => {
            let bytes = glb(&gltf_with_buffer(reference));
            fs::write(u.job().join("in.glb"), bytes).unwrap();
            "in.glb"
        }
        Kind::GlbImage => {
            uv_buffer(u);
            let bytes = glb(&gltf_with_image(reference));
            fs::write(u.job().join("in.glb"), bytes).unwrap();
            "in.glb"
        }
        Kind::Buffer => {
            u.write("in.gltf", &gltf_with_buffer(reference));
            "in.gltf"
        }
        Kind::Image => {
            uv_buffer(u);
            u.write("in.gltf", &gltf_with_image(reference));
            "in.gltf"
        }
        Kind::Mtllib => {
            u.write(
                "in.obj",
                &format!("mtllib {reference}\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n"),
            );
            "in.obj"
        }
        Kind::MapKd => {
            u.write("in.obj", TEXTURED_OBJ);
            u.write("in.mtl", &format!("newmtl m\nmap_Kd {reference}\n"));
            "in.obj"
        }
        Kind::Vrml => {
            u.write("in.wrl", &vrml_with_url(reference));
            "in.wrl"
        }
    }
}
#[derive(Clone, Copy, Debug)]
enum Kind {
    Buffer,
    Image,
    GlbBuffer,
    GlbImage,
    Mtllib,
    MapKd,
    Vrml,
}
const KINDS: [(Kind, &str); 7] = [
    (Kind::Buffer, "secret.bin"),
    (Kind::Image, "secret.png"),
    (Kind::GlbBuffer, "secret.bin"),
    (Kind::GlbImage, "secret.png"),
    (Kind::Mtllib, "secret.mtl"),
    (Kind::MapKd, "secret.png"),
    (Kind::Vrml, "secret.png"),
];
/// Names `secret` after what the file's kind expects, at `SECRET`, and beside it `EXT` for the
/// model's own file of that kind.
fn spelled(reference: &str, secret: &str) -> String {
    let extension = secret.rsplit('.').next().unwrap();
    reference
        .replace("SECRET", secret)
        .replace("EXT", extension)
}
/// A file of the kind `name` says, told from the one in `outside` by its content: the buffer has
/// no canary, the image is 8 by 8 rather than 4 by 4, and the library has a different material.
fn write_own_copy(u: &Upload, name: &str, like: &str) {
    let path = u.job().join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    match like.rsplit('.').next().unwrap() {
        "bin" => fs::write(path, positions()).unwrap(),
        "mtl" => fs::write(path, "newmtl own\nKd 0 1 0\n").unwrap(),
        _ => {
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::RgbImage::new(8, 8)
                .write_to(&mut bytes, image::ImageFormat::Png)
                .unwrap();
            fs::write(path, bytes.into_inner()).unwrap();
        }
    }
}
/// Percent-decoded, as whatever reads a published glTF will do it once.
fn decoded_uri(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| uri.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        out.push(hex.unwrap_or(bytes[i]));
        i += if hex.is_some() { 3 } else { 1 };
    }
    String::from_utf8(out).unwrap()
}
/// What makes an output safe to publish: every URI in it leads to a file beneath its directory.
#[track_caller]
fn assert_self_contained(output: &std::path::Path) {
    let root: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    let dir = fs::canonicalize(output.parent().unwrap()).unwrap();
    for list in ["buffers", "images"] {
        for item in root[list].as_array().into_iter().flatten() {
            let Some(uri) = item["uri"].as_str() else {
                continue;
            };
            let file = decoded_uri(uri);
            assert!(!file.contains('\\') && !file.starts_with('/'), "{uri}");
            let file = fs::canonicalize(dir.join(file)).expect(uri);
            assert!(file.starts_with(&dir), "{uri}");
        }
    }
}
#[test]
fn what_stays_in_the_folder_is_followed_however_it_is_written() {
    let uri_only = |kind| {
        matches!(
            kind,
            Kind::Buffer | Kind::Image | Kind::GlbBuffer | Kind::GlbImage
        )
    };
    for (kind, secret) in KINDS {
        let mut written = vec!["tex/a.EXT", "./tex/../tex/a.EXT", "tex\\a.EXT"];
        if uri_only(kind) {
            written.push("tex%2Fa.EXT");
        }
        for reference in written {
            let u = Upload::new();
            let name = spelled("tex/a.EXT", secret);
            write_own_copy(&u, &name, secret);
            let input = write_reference(&u, kind, &spelled(reference, secret));
            u.assert_followed_and_contained(input, &format!("{kind:?} {reference}"));
        }
    }
}
#[cfg(unix)]
#[test]
fn a_link_that_stays_in_the_folder_is_followed() {
    for (kind, secret) in KINDS {
        let u = Upload::new();
        write_own_copy(&u, &spelled("tex/a.EXT", secret), secret);
        std::os::unix::fs::symlink(spelled("tex/a.EXT", secret), u.job().join("inner-link"))
            .unwrap();
        let input = write_reference(&u, kind, "inner-link");
        u.assert_followed_and_contained(input, &format!("{kind:?}"));
    }
}
#[test]
fn what_leaves_the_folder_is_refused_however_it_is_written() {
    for (kind, secret) in KINDS {
        for reference in [
            "/etc/passwd",
            "C:\\Windows\\win.ini",
            "C:/Windows/win.ini",
            "C:win.ini",
            "\\\\server\\share\\a.EXT",
            "file:///etc/passwd",
            "http://x/y.png",
            "../outside/SECRET",
            "tex/../../outside/SECRET",
            "..\\outside\\SECRET",
            "..",
            "tex/../..",
            // Comes back inside, but only by naming the folder, which would let a file learn it.
            "../job/tex/a.EXT",
            "..%2Foutside%2FSECRET",
            "%2e%2e/outside/SECRET",
            "%252e%252e/outside/SECRET",
            "a%00b",
            "missing.EXT",
            "tex",
            "tex/",
            ".",
        ] {
            let u = Upload::new();
            write_own_copy(&u, &spelled("tex/a.EXT", secret), secret);
            let input = write_reference(&u, kind, &spelled(reference, secret));
            u.assert_refused(input);
        }
    }
}
#[cfg(unix)]
#[test]
fn a_link_is_judged_by_where_it_leads() {
    use std::os::unix::fs::symlink;
    for (kind, secret) in KINDS {
        let u = Upload::new();
        write_own_copy(&u, &spelled("tex/a.EXT", secret), secret);
        // `dir-link/../a.EXT` reads as a file in the model's folder, but the kernel goes through the
        // link and finds the one beside `outside`.
        let beside_outside = u.0 .0.join(spelled("a.EXT", secret));
        fs::copy(u.0 .0.join("outside").join(secret), beside_outside).unwrap();
        symlink(u.0 .0.join("outside"), u.job().join("dir-link")).unwrap();
        symlink(
            u.0 .0.join("outside").join(secret),
            u.job().join("file-link"),
        )
        .unwrap();
        symlink("/etc", u.job().join("etc-link")).unwrap();
        symlink("loop", u.job().join("loop")).unwrap();
        for reference in [
            "file-link",
            "dir-link/SECRET",
            "dir-link/../a.EXT",
            "etc-link/passwd",
            "loop",
            // The lookup by name is held to the same checks.
            "C:\\Users\\bob\\file-link",
            "/home/bob/file-link",
        ] {
            let input = write_reference(&u, kind, &spelled(reference, secret));
            u.assert_refused(input);
        }
    }
}
#[cfg(unix)]
#[test]
fn a_folder_named_like_a_scheme_does_not_make_a_url_a_path() {
    // Published as it reads, this would send whatever opens the output to fetch it.
    for (kind, secret) in KINDS {
        for reference in ["http://x/SECRET", "file:SECRET"] {
            let u = Upload::new();
            write_own_copy(&u, &format!("http:/x/{secret}"), secret);
            write_own_copy(&u, &format!("file:{secret}"), secret);
            let input = write_reference(&u, kind, &spelled(reference, secret));
            u.assert_refused(input);
        }
    }
}
#[cfg(unix)]
#[test]
fn a_colon_in_a_file_name_is_only_part_of_its_name() {
    for (kind, secret) in KINDS {
        let u = Upload::new();
        let name = spelled("x:y.EXT", secret);
        write_own_copy(&u, &name, secret);
        // A folder in front is what makes this a path rather than a scheme.
        let input = write_reference(&u, kind, &format!("./{name}"));
        u.assert_followed_and_contained(input, &format!("{kind:?}"));
    }
    let u = Upload::new();
    write_own_copy(&u, "t.png", "t.png");
    u.write("in.obj", TEXTURED_OBJ);
    u.write("in.mtl", "newmtl m\nmap_Kd t.png\n");
    u.write("a:b.obj", TRIANGLE);
    for args in [
        ["import", "in.obj", "-o", "scan:1.gltf", "--images", "copy"].as_slice(),
        &["import", "a:b.obj"],
    ] {
        let out = u.run(args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {stderr}");
    }
    assert_self_contained(&u.job().join("scan:1.gltf"));
    assert_self_contained(&u.job().join("a:b.gltf"));
}
#[test]
fn every_kind_of_reference_stays_in_the_folder() {
    for (kind, secret) in KINDS {
        let u = Upload::new();
        let relative = format!("../outside/{secret}");
        let input = write_reference(&u, kind, &relative);
        u.assert_refused(input);
        u.assert_followed_when_trusted(input);
        let u = Upload::new();
        let absolute = fs::canonicalize(u.0 .0.join("outside").join(secret)).unwrap();
        // Forward slashes, as a file would have them: a URI can't contain a raw backslash.
        let absolute = absolute.to_str().unwrap().replace('\\', "/");
        u.names_a_real_path();
        let input = write_reference(&u, kind, &absolute);
        u.assert_refused(input);
        u.assert_followed_when_trusted(input);
    }
}
#[test]
fn a_mtl_inside_cannot_reach_out_either() {
    let u = Upload::new();
    fs::create_dir(u.job().join("sub")).unwrap();
    u.write("in.obj", &TEXTURED_OBJ.replace("in.mtl", "sub/in.mtl"));
    u.write("sub/in.mtl", "newmtl m\nmap_Kd ../../outside/secret.png\n");
    u.assert_refused("in.obj");
}
#[test]
fn a_mtl_in_a_subfolder_can_climb_as_far_as_the_model() {
    let u = Upload::new();
    fs::create_dir(u.job().join("sub")).unwrap();
    image::RgbImage::new(4, 4)
        .save(u.job().join("t.png"))
        .unwrap();
    u.write("in.obj", &TEXTURED_OBJ.replace("in.mtl", "sub/in.mtl"));
    u.write("sub/in.mtl", "newmtl m\nmap_Kd ../t.png\n");
    let out = u.run(&["import", "in.obj", "-o", "out.gltf"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
#[test]
fn encoded_uris_are_decoded_exactly_once() {
    for uri in [
        "..%2Foutside%2Fsecret.bin",
        "%2e%2e/outside/secret.bin",
        "%252e%252e/outside/secret.bin",
        "m%00.bin",
    ] {
        let u = Upload::new();
        let input = write_reference(&u, Kind::Buffer, uri);
        u.assert_refused(input);
    }
    // Not a reference at all, so it isn't given the same answer, and nothing is looked up.
    for uri in ["%zz", "%", "m.bin%"] {
        let u = Upload::new();
        let input = write_reference(&u, Kind::Buffer, uri);
        for args in [
            ["import", input, "-o", "out.gltf"].as_slice(),
            &["inspect", input],
        ] {
            let stderr = u.stderr_of(args);
            assert!(stderr.contains("Error: "), "{uri}: {stderr}");
        }
    }
}
/// Each command says the same about both, apart from the reference as written.
#[track_caller]
fn assert_look_the_same(kind: Kind, forbidden: &str, missing: &str, prepare: impl Fn(&Upload)) {
    let u = Upload::new();
    prepare(&u);
    for args in [
        ["import", "in", "-o", "out.gltf"].as_slice(),
        &["inspect", "in"],
    ] {
        let run = |reference: &str| {
            let input = write_reference(&u, kind, reference);
            let args: Vec<&str> = args
                .iter()
                .map(|a| if *a == "in" { input } else { a })
                .collect();
            u.stderr_of(&args)
        };
        let told = run(forbidden).replace(forbidden, missing);
        assert_eq!(
            told,
            run(missing),
            "{kind:?}: {forbidden} against {missing}"
        );
    }
}
#[test]
fn a_missing_file_and_a_forbidden_one_look_the_same() {
    for (kind, secret) in KINDS {
        let forbidden = format!("../outside/{secret}");
        let missing = format!("../outside/{}", secret.replace("secret", "absent"));
        assert_look_the_same(kind, &forbidden, &missing, |_| {});
        // Whether it exists at all can't be told from what is said about its name either.
        assert_look_the_same(kind, "/etc/passwd", "/etc/absent", |_| {});
        // A directory is there, and isn't a file.
        assert_look_the_same(kind, "tex", "absent", |u: &Upload| {
            fs::create_dir(u.job().join("tex")).unwrap();
        });
    }
}
#[cfg(unix)]
#[test]
fn what_a_link_or_special_file_is_cant_be_told_from_a_missing_one() {
    use std::os::unix::fs::symlink;
    for (kind, secret) in KINDS {
        assert_look_the_same(kind, "file-link", "absent", |u: &Upload| {
            symlink(
                u.0 .0.join("outside").join(secret),
                u.job().join("file-link"),
            )
            .unwrap();
        });
        assert_look_the_same(kind, "dangling", "absent", |u: &Upload| {
            symlink(u.0 .0.join("nothing"), u.job().join("dangling")).unwrap();
        });
        assert_look_the_same(kind, "fifo", "absent", |u: &Upload| {
            let fifo = std::ffi::CString::new(u.job().join("fifo").to_str().unwrap()).unwrap();
            // SAFETY: `fifo` is a valid C string for the duration of the call.
            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        });
        assert_look_the_same(kind, "/dev/zero", "/dev/absent", |_| {});
    }
}
#[test]
fn a_vrml_file_name_cannot_add_a_library_to_the_generated_obj() {
    // The generated OBJ records the input's name in a comment, which a newline would end.
    let u = Upload::new();
    let name = "in\nmtllib ../../outside/secret.mtl\n.wrl";
    if fs::write(u.job().join(name), vrml_with_url("t.png")).is_err() {
        return; // Windows doesn't allow this name.
    }
    image::RgbImage::new(4, 4)
        .save(u.job().join("t.png"))
        .unwrap();
    let stderr = u.stderr_of(&["import", name, "-o", "out.gltf"]);
    assert!(stderr.contains(REFUSED), "{stderr}");
}
#[cfg(unix)]
#[test]
fn symlinks_and_special_files_are_refused_and_never_hang() {
    use std::os::unix::fs::symlink;
    let outside = |u: &Upload, name: &str| u.0 .0.join("outside").join(name);
    for (kind, secret) in KINDS {
        let u = Upload::new();
        symlink(outside(&u, secret), u.job().join("file-link")).unwrap();
        u.assert_refused(write_reference(&u, kind, "file-link"));
        let u = Upload::new();
        symlink(u.0 .0.join("outside"), u.job().join("dir-link")).unwrap();
        let reference = format!("dir-link/{secret}");
        u.assert_refused(write_reference(&u, kind, &reference));
        for special in ["/dev/zero", "/dev/stdin", "/proc/self/environ"] {
            let u = Upload::new();
            u.assert_refused(write_reference(&u, kind, special));
        }
        let u = Upload::new();
        let fifo = std::ffi::CString::new(u.job().join("fifo").to_str().unwrap()).unwrap();
        // SAFETY: `fifo` is a valid C string for the duration of the call.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        u.assert_refused(write_reference(&u, kind, "fifo"));
    }
}
#[cfg(unix)]
#[test]
fn links_are_followed_when_trusted() {
    use std::os::unix::fs::symlink;
    for (kind, secret) in KINDS {
        let u = Upload::new();
        symlink(
            u.0 .0.join("outside").join(secret),
            u.job().join("file-link"),
        )
        .unwrap();
        u.assert_followed_when_trusted(write_reference(&u, kind, "file-link"));
        let u = Upload::new();
        symlink(u.0 .0.join("outside"), u.job().join("dir-link")).unwrap();
        let reference = format!("dir-link/{secret}");
        u.assert_followed_when_trusted(write_reference(&u, kind, &reference));
    }
}
#[test]
fn a_trusted_reference_fails_with_the_real_reason() {
    for (kind, _) in KINDS {
        let u = Upload::new();
        let input = write_reference(&u, kind, "missing.png");
        for args in [
            ["import", input, "-o", "out.gltf", "--trusted"].as_slice(),
            &["inspect", input, "--trusted"],
        ] {
            // What the system said, rather than the refusal.
            let stderr = u.stderr_of(args);
            assert!(stderr.contains("os error"), "{kind:?}: {stderr}");
            assert!(!stderr.contains("outside"), "{kind:?}: {stderr}");
        }
    }
}
#[test]
fn a_name_that_looks_encoded_stays_a_name() {
    // A real file in the folder. Decoded a second time on the way back in, it would lead out.
    let name = |secret: &str| format!("..%2Foutside%2F{secret}");
    for (kind, secret) in KINDS {
        let u = Upload::new();
        write_own_copy(&u, &name(secret), secret);
        // A URI has to encode its own percent signs to name this file.
        let uri = matches!(
            kind,
            Kind::Buffer | Kind::Image | Kind::GlbBuffer | Kind::GlbImage
        );
        let reference = if uri {
            name(secret).replace('%', "%25")
        } else {
            name(secret)
        };
        let input = write_reference(&u, kind, &reference);
        u.assert_followed_and_contained(input, &format!("{kind:?}"));
    }
}
#[test]
fn an_image_an_extension_selects_stays_in_the_folder() {
    for extension in ["EXT_texture_webp", "EXT_texture_avif", "KHR_texture_basisu"] {
        let u = Upload::new();
        uv_buffer(&u);
        write_own_copy(&u, "own.png", "own.png");
        let mut root: Value = serde_json::from_str(&gltf_with_image("own.png")).unwrap();
        root["images"]
            .as_array_mut()
            .unwrap()
            .push(json!({"uri": "../outside/secret.png"}));
        root["textures"][0]["extensions"] = json!({extension: {"source": 1}});
        root["extensionsUsed"] = json!([extension]);
        u.write("in.gltf", &root.to_string());
        u.assert_refused("in.gltf");
    }
}
#[test]
fn every_texture_slot_stays_in_the_folder() {
    for (slot, material) in [
        ("normal", json!({"normalTexture": {"index": 0}})),
        ("occlusion", json!({"occlusionTexture": {"index": 0}})),
        ("emissive", json!({"emissiveTexture": {"index": 0}})),
        (
            "metallic",
            json!({"pbrMetallicRoughness": {"metallicRoughnessTexture": {"index": 0}}}),
        ),
        (
            "extension",
            json!({"extensions": {"KHR_materials_clearcoat": {"clearcoatTexture": {"index": 0}}}}),
        ),
    ] {
        let u = Upload::new();
        uv_buffer(&u);
        let mut root: Value =
            serde_json::from_str(&gltf_with_image("../outside/secret.png")).unwrap();
        root["materials"] = json!([material]);
        root["extensionsUsed"] = json!(["KHR_materials_clearcoat"]);
        u.write("in.gltf", &root.to_string());
        let stderr = u.stderr_of(&["import", "in.gltf", "-o", "out.gltf"]);
        assert!(stderr.contains(REFUSED), "{slot}: {stderr}");
    }
}
#[test]
fn an_image_nothing_uses_is_not_published() {
    let u = Upload::new();
    uv_buffer(&u);
    let mut root: Value = serde_json::from_str(&gltf_with_image("../outside/secret.png")).unwrap();
    root["materials"] = json!([{}]);
    root["textures"] = json!([]);
    u.write("in.gltf", &root.to_string());
    let out = u.run(&["import", "in.gltf", "-o", "out.gltf"]);
    assert_eq!(out.status.code(), Some(0));
    let output: Value =
        serde_json::from_slice(&fs::read(u.job().join("out.gltf")).unwrap()).unwrap();
    assert!(
        output["images"].as_array().is_none_or(Vec::is_empty),
        "{output}"
    );
}
#[test]
fn nothing_in_a_model_is_run_or_included() {
    // OBJ can name a command to run and another file to read, and VRML another model to read.
    let u = Upload::new();
    u.write(
        "in.obj",
        &format!("csh touch pwned\ncall ../outside/secret.obj\n{TRIANGLE}"),
    );
    u.write(
        "in.wrl",
        &format!(
            "#VRML V2.0 utf8\nInline {{ url \"../outside/secret.wrl\" }}\n{}",
            vrml_with_url("../outside/secret.png").replace("../outside/secret.png", "t.png")
        ),
    );
    write_own_copy(&u, "t.png", "t.png");
    for args in [
        ["import", "in.obj", "-o", "obj.gltf"].as_slice(),
        &["inspect", "in.obj"],
        &["import", "in.wrl", "-o", "wrl.gltf"],
        &["inspect", "in.wrl"],
    ] {
        let out = u.run(args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {stderr}");
    }
    assert!(!u.job().join("pwned").exists());
}
#[test]
fn a_line_added_through_a_vrml_url_is_held_to_the_same_rules() {
    write_vrml_url_lines("t.png\nmap_Kd ../outside/secret.png");
    write_vrml_url_lines("t.png\r\nmap_Kd ../outside/secret.png");
}
fn write_vrml_url_lines(url: &str) {
    // A string can hold a newline, which then starts a line of the generated library.
    let u = Upload::new();
    write_own_copy(&u, "t.png", "t.png");
    u.write("in.wrl", &vrml_with_url(url));
    u.assert_refused("in.wrl");
}
#[cfg(unix)]
#[test]
fn a_link_planted_at_an_output_name_is_not_written_through() {
    use std::os::unix::fs::symlink;
    for (planted, leads_to) in [
        ("out.gltf", "written.gltf"),
        ("out.bin", "written.bin"),
        ("out_texture_0.png", "written.png"),
        // One that exists is as tempting as one that doesn't.
        ("out.gltf", "secret.bin"),
        ("out.bin", "secret.bin"),
        ("out_texture_0.png", "secret.bin"),
    ] {
        let u = Upload::new();
        write_own_copy(&u, "t.png", "t.png");
        u.write("in.obj", TEXTURED_OBJ);
        u.write("in.mtl", "newmtl m\nmap_Kd t.png\n");
        let outside = u.0 .0.join("outside");
        let before = fs::read(outside.join("secret.bin")).unwrap();
        symlink(outside.join(leads_to), u.job().join(planted)).unwrap();
        // Copied, so that the texture has a name to write to.
        let out = u.run(&["import", "in.obj", "-o", "out.gltf", "--images", "copy"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Seen before starting if it leads somewhere, and when writing if it doesn't.
        assert!(
            matches!(out.status.code(), Some(1 | 4)),
            "{planted} to {leads_to}: {stderr}"
        );
        assert_eq!(fs::read(outside.join("secret.bin")).unwrap(), before);
    }
}
#[test]
fn inspecting_an_obj_reads_its_own_intermediate_back() {
    let u = Upload::new();
    image::RgbImage::new(4, 4)
        .save(u.job().join("t.png"))
        .unwrap();
    u.write("in.obj", TEXTURED_OBJ);
    u.write("in.mtl", "newmtl m\nmap_Kd t.png\n");
    let out = u.run(&["inspect", "in.obj", "--json", "--tmp-dir", ".."]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["textures"][0]["width"], 4, "{report}");
}
#[test]
fn a_vrml_texture_is_found_when_the_workspace_is_elsewhere() {
    let u = Upload::new();
    image::RgbImage::new(4, 4)
        .save(u.job().join("t.png"))
        .unwrap();
    u.write("in.wrl", &vrml_with_url("t.png"));
    let out = u.run(&["import", "in.wrl", "-o", "out.gltf", "--tmp-dir", ".."]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
}
/// The three spellings an absolute reference takes, with the name it comes down to.
const ABSOLUTE: [(&str, &str); 3] = [
    ("C:\\Users\\bob\\tex.EXT", "tex.EXT"),
    ("/home/bob/tex.EXT", "tex.EXT"),
    ("/etc/passwd", "passwd"),
];
#[test]
fn an_absolute_path_opens_the_file_of_that_name_beside_the_model() {
    for (kind, secret) in KINDS {
        for (written, name) in ABSOLUTE {
            let u = Upload::new();
            write_own_copy(&u, &spelled(name, secret), secret);
            let input = write_reference(&u, kind, &spelled(written, secret));
            let case = format!("{kind:?} {written}");
            u.assert_followed_and_contained(input, &case);
        }
    }
}
#[test]
fn an_absolute_path_without_that_name_beside_the_model_is_refused() {
    for (kind, secret) in KINDS {
        for (written, _) in ABSOLUTE {
            let u = Upload::new();
            // Under a folder of its name, and elsewhere: neither is beside the model.
            write_own_copy(&u, &spelled("tex/tex.EXT", secret), secret);
            write_own_copy(&u, &spelled("tex/passwd", secret), secret);
            let input = write_reference(&u, kind, &spelled(written, secret));
            u.assert_refused(input);
        }
    }
}
#[test]
fn a_real_absolute_path_only_ever_opens_the_copy_in_the_folder() {
    for (kind, secret) in KINDS {
        let u = Upload::new();
        let outside = fs::canonicalize(u.0 .0.join("outside").join(secret)).unwrap();
        write_own_copy(&u, secret, secret);
        let written = outside.to_str().unwrap().replace('\\', "/");
        u.names_a_real_path();
        let input = write_reference(&u, kind, &written);
        u.assert_followed_and_contained(input, &format!("{kind:?}"));
        for output in ["auto.bin", "keep.bin", "copy.bin"] {
            let bytes = fs::read(u.job().join(output)).unwrap();
            assert!(
                !bytes.windows(CANARY.len()).any(|w| w == CANARY),
                "{kind:?}"
            );
        }
    }
}
#[test]
fn a_name_is_looked_for_beside_the_file_that_wrote_it() {
    let model = TEXTURED_OBJ.replace("in.mtl", "sub/in.mtl");
    // The library is in a subfolder, and the texture is beside the model.
    let u = Upload::new();
    fs::create_dir(u.job().join("sub")).unwrap();
    write_own_copy(&u, "t.png", "t.png");
    u.write("in.obj", &model);
    u.write("sub/in.mtl", "newmtl m\nmap_Kd C:\\Users\\bob\\t.png\n");
    u.assert_refused("in.obj");
    // Beside the library.
    let u = Upload::new();
    write_own_copy(&u, "sub/t.png", "t.png");
    u.write("in.obj", &model);
    u.write("sub/in.mtl", "newmtl m\nmap_Kd C:\\Users\\bob\\t.png\n");
    u.assert_followed_and_contained("in.obj", "beside the library");
}
#[test]
fn a_relative_path_is_not_looked_for_by_name() {
    for (kind, secret) in KINDS {
        let u = Upload::new();
        write_own_copy(&u, secret, secret);
        let input = write_reference(&u, kind, &format!("sub/dir/{secret}"));
        u.assert_refused(input);
    }
}
#[test]
fn a_library_named_by_an_absolute_path_with_spaces_is_found() {
    let u = Upload::new();
    u.write("in.mtl", "newmtl m\nKd 1 0 0\n");
    u.write(
        "in.obj",
        "mtllib C:\\My Files\\in.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nusemtl m\nf 1 2 3\n",
    );
    let out = u.run(&["import", "in.obj", "-o", "out.gltf"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let root: Value = serde_json::from_slice(&fs::read(u.job().join("out.gltf")).unwrap()).unwrap();
    assert!(root["materials"][0]["pbrMetallicRoughness"]["baseColorFactor"].is_array());
}
#[test]
fn keeping_images_never_writes_a_reference_outside_the_output_folder() {
    for input in ["in.obj", "in.gltf"] {
        let u = Upload::new();
        fs::create_dir(u.job().join("elsewhere")).unwrap();
        image::RgbImage::new(4, 4)
            .save(u.job().join("t.png"))
            .unwrap();
        u.write("in.mtl", "newmtl m\nmap_Kd t.png\n");
        u.write("in.obj", TEXTURED_OBJ);
        uv_buffer(&u);
        u.write("in.gltf", &gltf_with_image("t.png"));
        let target = "elsewhere/out.gltf";
        let keep = ["import", input, "-o", target, "--images", "keep"];
        let stderr = u.stderr_of(&keep);
        assert!(stderr.contains("--images keep"), "{stderr}");
        assert!(!u.job().join(target).exists());
        // Copied by default, so what's published stands alone.
        let out = u.run(&["import", input, "-o", target]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let root: Value = serde_json::from_slice(&fs::read(u.job().join(target)).unwrap()).unwrap();
        assert_eq!(root["images"][0]["uri"], "out_texture_0.png");
        // The same folder keeps working, and `--trusted` allows the rest.
        let out = u.run(&["import", input, "-o", "same.gltf", "--images", "keep"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let target = "elsewhere/trusted.gltf";
        let out = u.run(&[
            "import",
            input,
            "-o",
            target,
            "--images",
            "keep",
            "--trusted",
        ]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let root: Value = serde_json::from_slice(&fs::read(u.job().join(target)).unwrap()).unwrap();
        assert_eq!(root["images"][0]["uri"], "../t.png");
    }
}

const HOSTILE: &str = "\x1b[31m<img\x07\u{202e}\u{200b}";
/// `HOSTILE` as a message may show it.
const CLEANED: &str = "?[31m?img";
/// Nothing that could drive a terminal or read as markup, wherever it came from. That includes
/// the characters that reorder text or hide it without showing anything.
#[track_caller]
fn assert_harmless(stderr: &str) {
    let hostile = |c: char| {
        (c.is_control() && c != '\n')
            || matches!(c, '<' | '>' | '&')
            || matches!(c, '\u{ad}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}')
            || matches!(c, '\u{2060}'..='\u{206f}' | '\u{feff}')
    };
    assert!(!stderr.contains(hostile), "{stderr:?}");
}
const TRIANGLE: &str = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n";
type Files = Vec<(String, Vec<u8>)>;
fn file(name: &str, body: impl Into<Vec<u8>>) -> (String, Vec<u8>) {
    (name.to_owned(), body.into())
}
/// `HOSTILE` at each place a parser takes text from. The flag says the import's messages have to
/// show it, so that the check is known to reach the message and not a silent success.
fn hostile_text() -> Vec<(&'static str, Files, bool)> {
    let h = HOSTILE;
    let obj = |body: String| ("m.obj", vec![file("m.obj", body)], true);
    let mtl = |body: String, shown: bool| {
        let obj = format!("mtllib m.mtl\n{TRIANGLE}");
        let files = vec![file("m.obj", obj), file("m.mtl", body)];
        ("m.obj", files, shown)
    };
    let wrl = |body: String, shown: bool| ("m.wrl", vec![file("m.wrl", body)], shown);
    let gltf = |root: Value, shown: bool| {
        let files = vec![file("m.gltf", root.to_string()), file("m.bin", positions())];
        ("m.gltf", files, shown)
    };
    let triangle = || -> Value { serde_json::from_str(&gltf_with_buffer("m.bin")).unwrap() };
    let with = |edit: &dyn Fn(&mut Value)| {
        let mut root = triangle();
        edit(&mut root);
        root
    };
    let invisible = with(&|root| {
        root["materials"] = json!([{
            "name": h, "alphaMode": "BLEND",
            "pbrMetallicRoughness": {"baseColorFactor": [1, 1, 1, 0]},
        }]);
        root["meshes"][0]["primitives"][0]["material"] = json!(0);
    });
    let attribute = with(&|root| {
        root["accessors"].as_array_mut().unwrap().push(json!({
            "bufferView": 0, "componentType": 5126, "count": 2, "type": "VEC3",
        }));
        root["meshes"][0]["primitives"][0]["attributes"][h] = json!(1);
    });
    let corrupt_png = [b"\x89PNG\r\n\x1a\n", h.as_bytes()].concat();
    let mut corrupt_image: Value = serde_json::from_str(&gltf_with_image("t.png")).unwrap();
    corrupt_image["images"][0]["mimeType"] = json!(h);
    let mut cases = vec![
        obj(format!("v 1{h} 0 0\n")),
        obj(format!("{h} 1\n{TRIANGLE}")),
        obj(format!("usemtl {h}\n{TRIANGLE}")),
        obj(format!("mtllib {h}.mtl\n")),
        obj(format!("usemtl {}\n{TRIANGLE}", h.repeat(5_000))),
        (
            "m.obj",
            vec![file("m.obj", format!("v {}\n", h.repeat(200_000)))],
            false,
        ),
        (
            "m.obj",
            vec![file("m.obj", &b"usemtl \xff\xfe\x1b[31m\nv 0 0 \xff\n"[..])],
            false,
        ),
        mtl(format!("newmtl m\nKd {h}\n"), false),
        mtl(format!("newmtl m\nmap_Kd {h}.png\n"), true),
        mtl(format!("newmtl m\nmap_Kd -{h} 1 t.png\n"), true),
        mtl(format!("newmtl m\n{h} 1\n"), true),
        (
            "m.obj",
            vec![
                file("m.obj", format!("mtllib m.mtl\nusemtl {h}\n{TRIANGLE}")),
                file("m.mtl", format!("newmtl {h}\nd 0\n")),
            ],
            true,
        ),
        (
            "m.obj",
            vec![
                file("m.obj", format!("mtllib m.mtl\n{TRIANGLE}")),
                file("m.mtl", &b"newmtl \xff\x1b\nmap_Kd \xff\x1b.png\n"[..]),
            ],
            false,
        ),
        wrl(format!("#VRML V2.0 utf8\n{h} {{ }}\n"), false),
        wrl(format!("{h} {{ }}\n"), false),
        wrl(vrml_with_url(h), true),
        wrl(
            vrml_with_url("t.png").replace("coordIndex [0 1 2 -1]", &format!("coordIndex [{h}]")),
            false,
        ),
        (
            "m.wrl",
            vec![file("m.wrl", &b"#VRML V2.0 utf8\n\xff\x1b[31m { }\n"[..])],
            false,
        ),
        (
            "m.gltf",
            vec![file("m.gltf", format!("{{\"asset\": {h}"))],
            false,
        ),
        gltf(with(&|root| root["extensionsRequired"] = json!([h])), true),
        gltf(with(&|root| root["extensionsUsed"] = json!([h])), true),
        gltf(invisible, true),
        gltf(attribute, true),
        gltf(with(&|root| root["buffers"][0]["uri"] = json!(h)), false),
        gltf(serde_json::from_str(&gltf_with_image(h)).unwrap(), false),
        gltf(
            serde_json::from_str(&gltf_with_image(&format!("data:image/png;base64,{h}"))).unwrap(),
            false,
        ),
        (
            "m.gltf",
            vec![
                file("m.gltf", corrupt_image.to_string()),
                file("m.bin", [positions(), vec![0; 24]].concat()),
                file("t.png", corrupt_png),
            ],
            false,
        ),
        (
            "m.glb",
            vec![file("m.glb", [b"glTF\x02\0\0\0", h.as_bytes()].concat())],
            false,
        ),
    ];
    // Windows doesn't allow these names.
    if cfg!(unix) {
        let name: &'static str = format!("{h}.obj").leak();
        cases.push((name, vec![file(name, "v 0 0 oops\n")], true));
        let texture = format!("{h}.png");
        cases.push((
            "m.obj",
            vec![
                file("m.obj", TEXTURED_OBJ.replace("in.mtl", "m.mtl")),
                file("m.mtl", format!("newmtl m\nmap_Kd {texture}\n")),
                file(&texture, "not an image"),
            ],
            true,
        ));
    }
    cases
}
#[test]
fn no_parser_lets_text_from_the_file_reach_a_message_as_it_is() {
    for (input, files, shown) in hostile_text() {
        let u = Upload::new();
        for (name, body) in &files {
            fs::write(u.job().join(name), body).unwrap();
        }
        let out = u.run(&["import", input, "-o", "relative.gltf"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            stderr.contains(CLEANED),
            shown,
            "{input}: {files:?}: {stderr}"
        );
        u.run(&["inspect", input]);
        u.run(&["inspect", input, "--json"]);
        // Given as absolute paths, which is what a message would show if it printed one.
        let job = fs::canonicalize(u.job()).unwrap();
        let (input, output) = (job.join(input), job.join("absolute.gltf"));
        let (input, output) = (input.to_str().unwrap(), output.to_str().unwrap());
        u.run(&["import", input, "-o", output]);
        u.run(&["inspect", input]);
    }
}
#[test]
fn arguments_are_cleaned_too() {
    let u = Upload::new();
    u.write("m.obj", TRIANGLE);
    let option = format!("--{HOSTILE}");
    for args in [
        [HOSTILE].as_slice(),
        &["import", &option],
        &["inspect", &option],
        &["import", "m.obj", "-o", "out.gltf", &option],
    ] {
        let stderr = u.stderr_of(args);
        assert!(stderr.contains(CLEANED), "{args:?}: {stderr}");
    }
    let extension = format!("m.{HOSTILE}");
    for command in ["import", "inspect"] {
        let stderr = u.stderr_of(&[command, &extension]);
        assert!(stderr.contains(CLEANED), "{stderr}");
    }
}
#[cfg(unix)]
#[test]
fn an_output_name_is_cleaned_too() {
    let u = Upload::new();
    u.write("m.obj", TRIANGLE);
    let output = format!("{HOSTILE}.gltf");
    let out = u.run(&["import", "m.obj", "-o", &output]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(&format!("Wrote {CLEANED}")), "{stderr}");
}
#[test]
fn an_ordinary_name_reads_as_written_in_messages_and_the_report() {
    let u = Upload::new();
    let name = "l'église [v2]+#1.obj";
    u.write(name, "v 0 0 oops\n");
    let stderr = u.stderr_of(&["import", name, "-o", "out.gltf"]);
    assert!(stderr.contains(&format!("Error: {name}:1: ")), "{stderr}");
    uv_buffer(&u);
    write_own_copy(&u, "mur d'enceinte #1.png", "t.png");
    u.write("m.gltf", &gltf_with_image("mur%20d'enceinte%20%231.png"));
    let out = u.run(&["inspect", "m.gltf"]);
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains("mur d'enceinte #1.png"), "{report}");
}
#[test]
fn a_name_from_the_file_is_cleaned_and_cut() {
    let u = Upload::new();
    let long = "x".repeat(200);
    u.write(
        "m.obj",
        &format!("usemtl <{long}\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n"),
    );
    let out = u.run(&["import", "m.obj", "-o", "out.gltf"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("for example ?xxx"), "{stderr}");
    assert!(!stderr.contains(&"x".repeat(64)), "{stderr}");
}
#[cfg(unix)]
#[test]
fn the_report_of_inspect_cleans_texture_names_but_its_json_keeps_them() {
    let u = Upload::new();
    let name = "e\x1b[31m.png";
    uv_buffer(&u);
    image::RgbImage::new(4, 4)
        .save_with_format(u.job().join(name), image::ImageFormat::Png)
        .unwrap();
    u.write("m.gltf", &gltf_with_image("e%1B%5B31m.png"));
    let text = u.run(&["inspect", "m.gltf"]);
    let stderr = String::from_utf8_lossy(&text.stderr);
    assert_eq!(text.status.code(), Some(0), "{stderr}");
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("e?[31m.png"), "{text}");
    assert_harmless(&text);
    let json = u.run(&["inspect", "m.gltf", "--json"]);
    let json: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(json["textures"][0]["name"], name);
}
#[test]
fn the_report_of_inspect_cleans_the_name_an_embedded_image_gives_itself() {
    let u = Upload::new();
    uv_buffer(&u);
    image::RgbImage::new(4, 4)
        .save(u.job().join("t.png"))
        .unwrap();
    let png = fs::read(u.job().join("t.png")).unwrap();
    let mut root: Value = serde_json::from_str(&gltf_with_image("t.png")).unwrap();
    root["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(json!({"buffer": 1, "byteLength": png.len()}));
    root["buffers"]
        .as_array_mut()
        .unwrap()
        .push(json!({"uri": "t.png", "byteLength": png.len()}));
    root["images"] = json!([{"bufferView": 2, "mimeType": "image/png", "name": HOSTILE}]);
    u.write("m.gltf", &root.to_string());
    let out = u.run(&["inspect", "m.gltf"]);
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains(CLEANED), "{report}");
}
/// Run with absolute paths, which are what would show if a message printed them.
#[test]
fn no_message_shows_where_the_job_runs() {
    for (kind, _) in KINDS {
        let u = Upload::new();
        let input = write_reference(&u, kind, "missing.png");
        let job = fs::canonicalize(u.job()).unwrap();
        let (input, output) = (job.join(input), job.join("out.gltf"));
        let (input, output) = (input.to_str().unwrap(), output.to_str().unwrap());
        u.run(&["import", input, "-o", output]);
        u.run(&["inspect", input]);
    }
}
#[test]
fn a_finished_import_names_its_output_without_the_folder() {
    let u = Upload::new();
    u.write("m.obj", "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n");
    let job = fs::canonicalize(u.job()).unwrap();
    let (input, output) = (job.join("m.obj"), job.join("out.gltf"));
    let out = u.run(&[
        "import",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("Wrote out.gltf ("), "{stderr}");
}
#[test]
fn an_error_in_a_bad_line_names_the_file_by_its_name() {
    let u = Upload::new();
    u.write("m.obj", "v 0 0 oops\n");
    let job = fs::canonicalize(u.job()).unwrap();
    let (input, output) = (job.join("m.obj"), job.join("out.gltf"));
    let args = [
        "import",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
    ];
    assert!(u.stderr_of(&args).contains("Error: m.obj:1: "));
}
#[test]
fn an_error_in_a_generated_file_names_the_vrml_it_came_from() {
    let u = Upload::new();
    u.write("m.wrl", &vrml_with_url("missing.png"));
    let stderr = u.stderr_of(&["import", "m.wrl", "-o", "out.gltf"]);
    assert!(
        stderr.contains("Error: m.wrl: Texture missing.png: "),
        "{stderr}"
    );
}
#[track_caller]
fn assert_few_lines(stderr: &str) {
    assert!(stderr.lines().count() <= 8, "{stderr}");
    assert!(stderr.len() < 2_000, "{} bytes", stderr.len());
}
#[test]
fn thousands_of_undefined_materials_are_one_warning() {
    let u = Upload::new();
    let mut obj = String::from("v 0 0 0\nv 1 0 0\nv 0 1 0\n");
    for i in 0..20_000 {
        obj.push_str(&format!("usemtl \x1b[31m{i}\nf 1 2 3\n"));
    }
    u.write("m.obj", &obj);
    let out = u.run(&["import", "m.obj", "-o", "out.gltf"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("undefined materials, using the default: 20000"),
        "{stderr}"
    );
    assert_few_lines(&stderr);
}
#[test]
fn thousands_of_unsupported_keywords_are_one_warning() {
    let u = Upload::new();
    let mut obj = String::from("v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n");
    for i in 0..20_000 {
        obj.push_str(&format!("keyword{i}\n"));
    }
    u.write("m.obj", &obj);
    let out = u.run(&["import", "m.obj", "-o", "out.gltf"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("unsupported OBJ records omitted: 20000"),
        "{stderr}"
    );
    assert_few_lines(&stderr);
}
/// Run with `HOSTILE` text in every name, so that volume and content are tested together.
#[track_caller]
fn assert_one_counted_warning(u: &Upload, input: &str, count: &str) {
    let out = u.run(&["import", input, "-o", "out.gltf"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.matches(count).count(), 1, "{stderr}");
    assert_few_lines(&stderr);
}
#[test]
fn thousands_of_unsupported_mtl_properties_are_one_warning() {
    let u = Upload::new();
    let mut library = String::new();
    let mut obj = format!("mtllib m.mtl\n{}", TRIANGLE.replace("f 1 2 3\n", ""));
    for i in 0..20_000 {
        library.push_str(&format!("newmtl {HOSTILE}{i}\nd 0\n{HOSTILE}{i} 1\n"));
        obj.push_str(&format!("usemtl {HOSTILE}{i}\nf 1 2 3\n"));
    }
    u.write("m.mtl", &library);
    u.write("m.obj", &obj);
    let out = u.run(&["import", "m.obj", "-o", "out.gltf"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unsupported MTL properties ignored: 20000"),
        "{stderr}"
    );
    assert!(stderr.contains("as opacity): 20000"), "{stderr}");
    assert_few_lines(&stderr);
}
#[test]
fn thousands_of_map_options_are_one_warning() {
    let u = Upload::new();
    let mut library = String::from("newmtl m\n");
    for _ in 0..20_000 {
        library.push_str("map_Kd -t 1 t.png\n");
    }
    u.write("m.mtl", &library);
    u.write(
        "m.obj",
        &format!("mtllib m.mtl\n{TEXTURED_OBJ}").replace("mtllib in.mtl\n", ""),
    );
    write_own_copy(&u, "t.png", "t.png");
    assert_one_counted_warning(&u, "m.obj", "unsupported map_Kd options ignored: 20000");
}
#[test]
fn thousands_of_objects_say_nothing_each() {
    let u = Upload::new();
    let mut obj = String::from("v 0 0 0\nv 1 0 0\nv 0 1 0\n");
    for i in 0..20_000 {
        obj.push_str(&format!("o {HOSTILE}{i}\nf 1 2 3\n"));
    }
    u.write("m.obj", &obj);
    let out = u.run(&["import", "m.obj", "-o", "out.gltf"]);
    assert_few_lines(&String::from_utf8_lossy(&out.stderr));
}
#[test]
fn thousands_of_faces_too_short_are_one_warning() {
    let u = Upload::new();
    let short = "f 1 2\n".repeat(20_000);
    u.write("m.obj", &format!("{TRIANGLE}{short}"));
    assert_one_counted_warning(&u, "m.obj", "omitted 20000 faces");
}
/// A glTF of one triangle and `count` copies of a material, each with its own primitive.
fn gltf_with_materials(u: &Upload, count: usize, material: impl Fn(usize) -> Value) {
    fs::write(u.job().join("m.bin"), positions()).unwrap();
    let mut root: Value = serde_json::from_str(&gltf_with_buffer("m.bin")).unwrap();
    root["materials"] = (0..count).map(material).collect();
    let primitives: Vec<Value> = (0..count)
        .map(|i| json!({"attributes": {"POSITION": 0}, "material": i}))
        .collect();
    root["meshes"][0]["primitives"] = json!(primitives);
    u.write("m.gltf", &root.to_string());
}
#[test]
fn thousands_of_invisible_gltf_materials_are_one_warning() {
    let u = Upload::new();
    gltf_with_materials(&u, 20_000, |i| {
        json!({
            "name": format!("{HOSTILE}{i}"), "alphaMode": "BLEND",
            "pbrMetallicRoughness": {"baseColorFactor": [1, 1, 1, 0]},
        })
    });
    assert_one_counted_warning(&u, "m.gltf", "which will be invisible: 20000");
}
#[test]
fn thousands_of_dropped_extensions_are_one_warning() {
    let u = Upload::new();
    gltf_with_materials(
        &u,
        20_000,
        |i| json!({"extensions": {format!("{HOSTILE}{i}"): {}}}),
    );
    assert_one_counted_warning(&u, "m.gltf", "extensions dropped from the output: 20000");
}
#[test]
fn thousands_of_unknown_extensions_are_one_warning_from_both_commands() {
    let u = Upload::new();
    fs::write(u.job().join("m.bin"), positions()).unwrap();
    let mut root: Value = serde_json::from_str(&gltf_with_buffer("m.bin")).unwrap();
    let names: Vec<String> = (0..20_000).map(|i| format!("{HOSTILE}{i}")).collect();
    root["extensionsUsed"] = json!(names);
    u.write("m.gltf", &root.to_string());
    assert_one_counted_warning(&u, "m.gltf", "unsupported extensions ignored: 20000");
    let out = u.run(&["inspect", "m.gltf"]);
    assert_eq!(out.stderr.iter().filter(|b| **b == b'\n').count(), 1);
}
#[test]
fn thousands_of_primitives_of_lines_are_one_warning() {
    let u = Upload::new();
    fs::write(u.job().join("m.bin"), positions()).unwrap();
    let mut root: Value = serde_json::from_str(&gltf_with_buffer("m.bin")).unwrap();
    let mut primitives = vec![json!({"attributes": {"POSITION": 0}})];
    primitives.extend((0..20_000).map(|_| json!({"attributes": {"POSITION": 0}, "mode": 1})));
    root["meshes"][0]["primitives"] = json!(primitives);
    u.write("m.gltf", &root.to_string());
    assert_one_counted_warning(&u, "m.gltf", "omitted 20000 primitives");
}
#[test]
fn thousands_of_bad_vrml_shapes_warn_once_each() {
    let u = Upload::new();
    let shape = "Shape { geometry IndexedFaceSet {\n\
                 coord Coordinate { point [0 0 0, 1 0 0, 0 1 0, 9] }\n\
                 coordIndex [0 1 -1 0 1 2 -1] color Color { color [1 0 0] } } }\n";
    u.write(
        "m.wrl",
        &format!("#VRML V2.0 utf8\n{}", shape.repeat(20_000)),
    );
    let out = u.run(&["import", "m.wrl", "-o", "out.gltf"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    for warning in [
        "partial tuple",
        "colours are omitted",
        "fewer than 3 vertices",
    ] {
        assert_eq!(stderr.matches(warning).count(), 1, "{stderr}");
    }
    assert_few_lines(&stderr);
}
#[test]
fn repeating_a_library_does_not_repeat_its_warning() {
    let u = Upload::new();
    u.write("m.mtl", "newmtl m\nodd 1\n");
    let libraries = "mtllib m.mtl\n".repeat(2_000);
    u.write(
        "m.obj",
        &format!("{libraries}v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n"),
    );
    let out = u.run(&["import", "m.obj", "-o", "out.gltf"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("unsupported MTL properties ignored: 2000"),
        "{stderr}"
    );
    assert_few_lines(&stderr);
}
#[test]
fn warnings_found_before_a_failure_still_show() {
    let u = Upload::new();
    u.write("m.obj", "odd 1\nv 0 0 oops\n");
    let stderr = u.stderr_of(&["import", "m.obj", "-o", "out.gltf"]);
    let warning = stderr
        .find("unsupported OBJ records omitted: 1")
        .expect(&stderr);
    assert!(warning < stderr.find("Error: ").expect(&stderr), "{stderr}");
}
#[test]
fn a_missing_input_is_named() {
    let u = Upload::new();
    for name in ["none.obj", "none.wrl", "none.gltf"] {
        let stderr = u.stderr_of(&["import", name, "-o", "out.gltf"]);
        assert!(stderr.starts_with(&format!("Error: {name}: ")), "{stderr}");
        assert_eq!(stderr.lines().count(), 1, "{stderr}");
    }
}
