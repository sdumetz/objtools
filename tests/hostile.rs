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
        u.names_a_real_path();
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
        assert_eq!(forbidden.replace("secret", "absent"), missing, "{kind:?}");
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
    u.names_a_real_path();
    let input = write_reference(&u, Kind::MapKd, &written);
    let out = u.run(&["inspect", input, "--json"]);
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    // The outside copy is 4 by 4.
    assert_eq!(report["textures"][0]["width"], 8, "{report}");
}
#[test]
fn a_relative_path_is_not_looked_for_by_name() {
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
    assert!(stderr.lines().count() <= 5, "{stderr}");
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
