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
fn obj_report_matches_report_of_imported_gltf() {
    let f = half_textured("source.png");
    let out = f.run(&["import"], "in.obj");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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
