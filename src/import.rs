//! Import orchestration. Geometry lives on disk, never in a whole-model structure.
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
pub type Res<T> = Result<T, Box<dyn std::error::Error>>;
static SEQ: AtomicU64 = AtomicU64::new(0);
pub struct Options {
    pub input: PathBuf,
    pub output: PathBuf,
    pub tmp_dir: PathBuf,
    pub center: bool,
    pub z_up: bool,
    pub progress: bool,
    pub keep_tmp: bool,
    pub images: crate::images::Options,
    /// Only warnings reach stderr; `inspect` imports into a workspace nobody will look at.
    pub quiet: bool,
    /// Drop OBJ vertex colours instead of writing `COLOR_0`/`_COLOR`.
    pub no_color: bool,
}
/// A private directory removed on drop, including when a command bails out early.
pub struct Workspace {
    pub path: PathBuf,
    keep: bool,
}
impl Workspace {
    pub fn create(tmp_dir: &std::path::Path, keep: bool) -> Res<Self> {
        let path = tmp_dir.join(format!(
            "objtools-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self { path, keep })
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        if self.keep {
            eprintln!("Kept workspace: {}", self.path.display());
        } else {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
pub fn run(opts: Options) -> Res<()> {
    let ext = opts
        .input
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(ext.as_str(), "obj" | "wrl" | "vrml") {
        return Err(format!("Unsupported input format: .{ext}").into());
    }
    if opts.output.extension().and_then(|s| s.to_str()) != Some("gltf") {
        return Err("Output must have a .gltf extension".into());
    }
    if opts.output.exists() || opts.output.with_extension("bin").exists() {
        return Err("Output .gltf or .bin already exists; choose a new output path".into());
    }
    let workspace = Workspace::create(&opts.tmp_dir, opts.keep_tmp)?;
    let source = if ext == "obj" {
        opts.input.clone()
    } else {
        let intermediate = workspace.path.join("source.obj");
        crate::vrml::convert(&opts.input, &intermediate, &workspace.path, opts.progress)?;
        intermediate
    };
    let texture_base = opts
        .input
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let scene = crate::obj::spool(
        &source,
        &workspace.path,
        opts.progress,
        if ext == "obj" {
            None
        } else {
            Some(texture_base)
        },
        !opts.no_color,
    )?;
    crate::gltf::write(scene, &workspace.path, &opts)
}
