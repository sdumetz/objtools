//! The binary against files built to hurt it, and the limits a host would run it under.
use serde_json::{json, Value};
#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt, process::ExitStatusExt};
use std::{
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
#[cfg(unix)]
fn write_face_bomb(f: &Fixture) {
    let mut obj = String::from("v 0 0 0\nv 1 0 0\nv 0 1 0\n");
    obj.push_str(&"f 1 2 3\n".repeat(200_000));
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
    assert_eq!(out.status.signal(), None, "{stderr}");
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("File too large"), "{stderr}");
    assert_eq!(f.entries(), inputs);
}
#[cfg(unix)]
#[test]
fn a_file_size_limit_stops_an_obj_import_and_leaves_nothing_behind() {
    let f = Fixture::new();
    write_face_bomb(&f);
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

/// A model in `job`, and in `outside` the files a hostile one would like to reach.
struct Upload(Fixture);
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
        Self(f)
    }
    fn job(&self) -> PathBuf {
        self.0 .0.join("job")
    }
    fn write(&self, name: &str, body: &str) {
        fs::write(self.job().join(name), body).unwrap();
    }
    /// Never waits for more than a minute, since what's being tested may be a hang.
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
        child.wait_with_output().unwrap()
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
            assert!(
                !name.starts_with("objtools-") && !name.starts_with("out"),
                "{name}"
            );
            // Links are skipped because an allowed read wouldn't follow them, and a FIFO would hang.
            if !fs::symlink_metadata(&path).unwrap().is_file() {
                continue;
            }
            let bytes = fs::read(&path).unwrap();
            assert!(!bytes.windows(CANARY.len()).any(|w| w == CANARY), "{name}");
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
/// Writes `in.*` referring to `reference` as one of the five kinds of reference, and returns its name.
fn write_reference(u: &Upload, kind: Kind, reference: &str) -> &'static str {
    match kind {
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
    Mtllib,
    MapKd,
    Vrml,
}
const KINDS: [(Kind, &str); 5] = [
    (Kind::Buffer, "secret.bin"),
    (Kind::Image, "secret.png"),
    (Kind::Mtllib, "secret.mtl"),
    (Kind::MapKd, "secret.png"),
    (Kind::Vrml, "secret.png"),
];
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
        let input = write_reference(&u, kind, &absolute);
        u.assert_refused(input);
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
}
/// Workspace names are random, and they reach stderr through a generated file's path.
fn without_workspace(stderr: &str) -> String {
    let parts = stderr
        .split('/')
        .filter(|part| !part.starts_with("objtools-"));
    parts.collect::<Vec<_>>().join("/")
}
#[test]
fn a_missing_file_and_a_forbidden_one_look_the_same() {
    for (kind, secret) in KINDS {
        let u = Upload::new();
        let input = write_reference(&u, kind, &format!("../outside/{secret}"));
        let forbidden = u.stderr_of(&["import", input, "-o", "out.gltf"]);
        // Same length as `secret`, so only the reference as written differs.
        let input = write_reference(
            &u,
            kind,
            &format!("../outside/{}", secret.replace("secret", "absent")),
        );
        let missing = u.stderr_of(&["import", input, "-o", "out.gltf"]);
        assert_eq!(
            without_workspace(&forbidden.replace("secret", "absent")),
            without_workspace(&missing),
            "{kind:?}"
        );
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
#[test]
fn an_absolute_path_from_an_old_exporter_finds_the_file_beside_the_model() {
    for (kind, secret) in KINDS {
        for written in [
            format!("C:\\Users\\bob\\{secret}"),
            format!("/home/bob/{secret}"),
        ] {
            let u = Upload::new();
            let name = u.job().join(secret);
            fs::copy(u.0 .0.join("outside").join(secret), &name).unwrap();
            let input = write_reference(&u, kind, &written);
            for args in [
                ["import", input, "-o", "out.gltf"].as_slice(),
                &["inspect", input],
            ] {
                let out = u.run(args);
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert!(out.status.success(), "{kind:?} {written}: {stderr}");
            }
        }
    }
}
#[test]
fn a_real_absolute_path_only_ever_opens_the_copy_in_the_folder() {
    let u = Upload::new();
    let outside = fs::canonicalize(u.0 .0.join("outside/secret.png")).unwrap();
    image::RgbImage::new(8, 8)
        .save(u.job().join("secret.png"))
        .unwrap();
    let written = outside.to_str().unwrap().replace('\\', "/");
    let input = write_reference(&u, Kind::MapKd, &written);
    let out = u.run(&["inspect", input, "--json"]);
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    // The outside copy is 4 by 4.
    assert_eq!(report["textures"][0]["width"], 8, "{report}");
}
#[test]
fn a_relative_path_is_not_retried_by_name() {
    for (kind, secret) in KINDS {
        let u = Upload::new();
        fs::copy(u.0 .0.join("outside").join(secret), u.job().join(secret)).unwrap();
        let input = write_reference(&u, kind, &format!("sub/dir/{secret}"));
        let stderr = u.stderr_of(&["import", input, "-o", "out.gltf"]);
        assert!(stderr.contains(REFUSED), "{kind:?}: {stderr}");
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
