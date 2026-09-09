//! VRML (`.wrl`) → Wavefront OBJ.
//!
//! VRML puts its geometry in `IndexedFaceSet` nodes, which is a poor match for OBJ in one
//! specific way: a face's position indices and its texture-coordinate indices live in two
//! separate arrays (`coordIndex` / `texCoordIndex`) that pair up positionally, and the file is
//! free to put them in either order. OBJ wants them woven together on one `f` line. So the index
//! arrays are spooled to fixed-width binary temp files as they stream past, and the faces are
//! written at the end of each `Shape`, when every array is known — the same trick `split` uses,
//! and for the same reason: it keeps memory flat regardless of how big the mesh is.
//!
//! Coordinates go through the fixed-point parser, so a georeferenced `.wrl` converts to OBJ
//! without losing a digit.

use std::fs::File;
use std::io::{BufReader, BufWriter, ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::fixed::{format_fixed_as_decimal, parse_decimal_fixed};
use crate::format::fmt_bytes;
use crate::import::ImportOptions;
use crate::tmp::TmpGuard;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Makes every spool file name unique within this process; the pid separates processes.
/// A per-shape counter is not enough: several conversions can run concurrently in one process
/// (Cargo's test harness does exactly that), and each would claim shape 1.
static SPOOL_SEQ: AtomicU64 = AtomicU64::new(0);

// ── lexer ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Tok {
    Word,
    Num,
    Str,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
}

/// Byte-level scanner over the input. VRML's arrays run to millions of tokens spread across
/// lines, so this reads bytes rather than lines and reuses one buffer for the token text.
struct Lexer {
    file: File,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
    eof: bool,
    pending: Option<u8>,
    text: Vec<u8>,
    consumed: u64,
}

impl Lexer {
    fn open(path: &str) -> Res<Self> {
        let file = File::open(path).map_err(|e| format!("{}: {}", path, e))?;
        Ok(Self {
            file,
            buf: vec![0u8; 256 * 1024],
            pos: 0,
            len: 0,
            eof: false,
            pending: None,
            text: Vec::with_capacity(64),
            consumed: 0,
        })
    }

    #[inline]
    fn get(&mut self) -> std::io::Result<Option<u8>> {
        if let Some(b) = self.pending.take() {
            return Ok(Some(b));
        }
        if self.pos == self.len {
            if self.eof {
                return Ok(None);
            }
            self.len = self.file.read(&mut self.buf)?;
            self.pos = 0;
            if self.len == 0 {
                self.eof = true;
                return Ok(None);
            }
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        self.consumed += 1;
        Ok(Some(b))
    }

    fn next_token(&mut self) -> std::io::Result<Option<Tok>> {
        loop {
            let b = match self.get()? {
                Some(b) => b,
                None => return Ok(None),
            };
            match b {
                // commas are whitespace in VRML
                b' ' | b'\t' | b'\r' | b'\n' | b',' => continue,
                b'#' => {
                    while let Some(c) = self.get()? {
                        if c == b'\n' {
                            break;
                        }
                    }
                }
                b'{' => return Ok(Some(Tok::LBrace)),
                b'}' => return Ok(Some(Tok::RBrace)),
                b'[' => return Ok(Some(Tok::LBracket)),
                b']' => return Ok(Some(Tok::RBracket)),
                b'"' => {
                    self.text.clear();
                    while let Some(c) = self.get()? {
                        if c == b'\\' {
                            if let Some(esc) = self.get()? {
                                self.text.push(esc);
                            }
                            continue;
                        }
                        if c == b'"' {
                            break;
                        }
                        self.text.push(c);
                    }
                    return Ok(Some(Tok::Str));
                }
                _ => {
                    self.text.clear();
                    self.text.push(b);
                    while let Some(c) = self.get()? {
                        match c {
                            b' ' | b'\t' | b'\r' | b'\n' | b',' => break,
                            b'{' | b'}' | b'[' | b']' | b'#' | b'"' => {
                                self.pending = Some(c);
                                break;
                            }
                            _ => self.text.push(c),
                        }
                    }
                    let first = self.text[0];
                    let numeric =
                        first.is_ascii_digit() || first == b'-' || first == b'+' || first == b'.';
                    return Ok(Some(if numeric { Tok::Num } else { Tok::Word }));
                }
            }
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.text).into_owned()
    }
}

// ── node / field classification ──────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Node {
    Shape,
    IndexedFaceSet,
    Coordinate,
    TextureCoordinate,
    Normal,
    Color,
    Material,
    ImageTexture,
    Transform,
    Other,
}

fn node_kind(name: &str) -> Node {
    match name {
        "Shape" => Node::Shape,
        "IndexedFaceSet" => Node::IndexedFaceSet,
        // Coordinate3 / TextureCoordinate2 are the VRML 1.0 spellings.
        "Coordinate" | "Coordinate3" => Node::Coordinate,
        "TextureCoordinate" | "TextureCoordinate2" => Node::TextureCoordinate,
        "Normal" => Node::Normal,
        "Color" => Node::Color,
        "Material" => Node::Material,
        "ImageTexture" | "Texture2" => Node::ImageTexture,
        "Transform" => Node::Transform,
        _ => Node::Other,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Array {
    Points,
    TexPoints,
    Normals,
    Colors,
    CoordIndex,
    TexCoordIndex,
    NormalIndex,
    Urls,
    Skip,
}

fn array_kind(parent: Node, field: &str) -> Array {
    match (parent, field) {
        (Node::Coordinate, "point") => Array::Points,
        (Node::TextureCoordinate, "point") => Array::TexPoints,
        (Node::Normal, "vector") => Array::Normals,
        (Node::Color, "color") => Array::Colors,
        (Node::IndexedFaceSet, "coordIndex") => Array::CoordIndex,
        (Node::IndexedFaceSet, "texCoordIndex") => Array::TexCoordIndex,
        (Node::IndexedFaceSet, "normalIndex") => Array::NormalIndex,
        (Node::ImageTexture, "url") => Array::Urls,
        _ => Array::Skip,
    }
}

enum Frame {
    Node(Node),
    Array,
}

// ── index spools ─────────────────────────────────────────────────────────────

/// One index array of one shape, written to disk as little-endian `i32`s (with VRML's `-1`
/// face terminators kept inline) so it can be replayed in lockstep with its siblings.
struct Spool {
    path: PathBuf,
    writer: BufWriter<File>,
}

impl Spool {
    fn create(dir: &std::path::Path, name: &str, guard: &mut TmpGuard) -> Res<Self> {
        let path = guard.track(dir.join(name)).clone();
        let writer = BufWriter::new(File::create(&path)?);
        Ok(Self { path, writer })
    }

    fn push(&mut self, i: i32) -> std::io::Result<()> {
        self.writer.write_all(&i.to_le_bytes())
    }

    fn finish(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

fn read_i32(r: &mut impl Read) -> std::io::Result<Option<i32>> {
    let mut b = [0u8; 4];
    match r.read_exact(&mut b) {
        Ok(()) => Ok(Some(i32::from_le_bytes(b))),
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e),
    }
}

/// Read one face's worth of indices, up to the `-1` terminator.
/// Returns false only at a clean end of stream.
fn read_face(r: &mut impl Read, out: &mut Vec<i32>) -> std::io::Result<bool> {
    out.clear();
    loop {
        match read_i32(r)? {
            None => return Ok(!out.is_empty()),
            Some(-1) => return Ok(true),
            Some(i) => out.push(i),
        }
    }
}

// ── materials ────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Material {
    name: String,
    diffuse: Option<[String; 3]>,
    specular: Option<[String; 3]>,
    shininess: Option<String>,
    transparency: Option<String>,
    texture: Option<String>,
}

impl Material {
    fn is_empty(&self) -> bool {
        self.diffuse.is_none()
            && self.specular.is_none()
            && self.shininess.is_none()
            && self.transparency.is_none()
            && self.texture.is_none()
    }
}

/// OBJ object and material names are whitespace-delimited, so whitespace has to go.
fn sanitize(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect();
    if out.is_empty() {
        "unnamed".to_string()
    } else {
        out
    }
}

// ── conversion ───────────────────────────────────────────────────────────────

#[derive(Default)]
struct Warned {
    colors: bool,
    transform: bool,
    ragged: bool,
    short_face: bool,
}

/// Per-`Shape` state. Everything here is O(1) in the size of the mesh; the geometry itself is
/// either already written to the output or spooled to disk.
struct Shape {
    name: String,
    v_base: u64,
    vt_base: u64,
    vn_base: u64,
    v_count: u64,
    vt_count: u64,
    vn_count: u64,
    coord: Option<Spool>,
    texcoord: Option<Spool>,
    normal: Option<Spool>,
    material: Material,
}

pub fn convert(opts: &ImportOptions) -> Res<()> {
    check_header(&opts.file_path)?;

    let mut lex = Lexer::open(&opts.file_path)?;
    let mut guard = TmpGuard::new(opts.keep_tmp);

    // The .mtl sits next to the .obj, so it only exists when the OBJ goes to a file.
    let mtl_path = opts
        .output
        .as_ref()
        .map(|o| PathBuf::from(o).with_extension("mtl"));

    let out: Box<dyn Write> = match &opts.output {
        Some(p) => Box::new(BufWriter::with_capacity(
            256 * 1024,
            File::create(p).map_err(|e| format!("{}: {}", p, e))?,
        )),
        None => Box::new(BufWriter::with_capacity(256 * 1024, std::io::stdout())),
    };
    let mut out = out;

    writeln!(out, "# Converted from {} by objtools", opts.file_path)?;
    if let Some(m) = &mtl_path {
        let name = m.file_name().map(|n| n.to_string_lossy().into_owned());
        if let Some(name) = name {
            writeln!(out, "mtllib {}", name)?;
        }
    }

    let mut stack: Vec<Frame> = Vec::new();
    let mut words: Vec<String> = Vec::new(); // last few words, for `DEF name Node {`
    let mut shape: Option<Shape> = None;
    let mut materials: Vec<Material> = Vec::new();
    let mut warned = Warned::default();

    let mut v_total: u64 = 0;
    let mut vt_total: u64 = 0;
    let mut vn_total: u64 = 0;
    let mut shape_count = 0usize;

    // A field like `diffuseColor 0.3 0.5 0.8` is bare numbers after a word, not an array.
    let mut collect: Option<(String, Vec<String>, usize)> = None;

    let mut next_progress: u64 = 100_000_000;

    while let Some(tok) = lex.next_token()? {
        if opts.progress && lex.consumed >= next_progress {
            next_progress = lex.consumed + 100_000_000;
            eprintln!("Read {}", fmt_bytes(lex.consumed));
        }

        match tok {
            Tok::Num => {
                if let Some((_, vals, want)) = &mut collect {
                    vals.push(lex.text());
                    if vals.len() == *want {
                        let (field, vals, _) = collect.take().unwrap();
                        apply_scalar_field(&field, &vals, &mut shape, &mut warned);
                    }
                }
            }

            Tok::Word => {
                collect = None;
                let w = lex.text();
                let current = current_node(&stack);
                match (current, w.as_str()) {
                    (Node::Material, "diffuseColor") | (Node::Material, "specularColor") => {
                        collect = Some((w.clone(), Vec::new(), 3));
                    }
                    (Node::Material, "shininess") | (Node::Material, "transparency") => {
                        collect = Some((w.clone(), Vec::new(), 1));
                    }
                    (Node::Transform, "translation") | (Node::Transform, "scale") => {
                        collect = Some((w.clone(), Vec::new(), 3));
                    }
                    _ => {}
                }
                words.push(w);
                if words.len() > 3 {
                    words.remove(0);
                }
            }

            Tok::Str => {
                // `url "file.jpg"` without brackets
                if current_node(&stack) == Node::ImageTexture && words.last().map(String::as_str) == Some("url") {
                    if let Some(s) = shape.as_mut() {
                        s.material.texture = Some(lex.text());
                    }
                }
            }

            Tok::LBrace => {
                let kind = words.last().map(|w| node_kind(w)).unwrap_or(Node::Other);
                if kind == Node::Shape {
                    shape_count += 1;
                    let name = def_name(&words)
                        .map(|n| sanitize(&n))
                        .unwrap_or_else(|| format!("shape_{}", shape_count));
                    writeln!(out, "o {}", name)?;
                    shape = Some(Shape {
                        name,
                        v_base: v_total,
                        vt_base: vt_total,
                        vn_base: vn_total,
                        v_count: 0,
                        vt_count: 0,
                        vn_count: 0,
                        coord: None,
                        texcoord: None,
                        normal: None,
                        material: Material::default(),
                    });
                }
                stack.push(Frame::Node(kind));
                words.clear();
            }

            Tok::RBrace => {
                if let Some(Frame::Node(kind)) = stack.pop() {
                    if kind == Node::Shape {
                        if let Some(s) = shape.take() {
                            finish_shape(s, &mut out, &mut materials, &mut guard, &mut warned)?;
                        }
                    }
                }
                words.clear();
            }

            Tok::LBracket => {
                let field = words.last().cloned().unwrap_or_default();
                let kind = array_kind(current_node(&stack), &field);
                match kind {
                    Array::Skip => stack.push(Frame::Array),
                    _ => {
                        read_array(
                            kind,
                            &mut lex,
                            &mut out,
                            &mut shape,
                            &mut v_total,
                            &mut vt_total,
                            &mut vn_total,
                            &opts.tmp_dir,
                            &mut guard,
                            &mut warned,
                        )?;
                    }
                }
                words.clear();
            }

            Tok::RBracket => {
                stack.pop();
                words.clear();
            }
        }
    }

    // A Shape left unclosed by a truncated file still deserves its faces.
    if let Some(s) = shape.take() {
        finish_shape(s, &mut out, &mut materials, &mut guard, &mut warned)?;
    }

    out.flush()?;

    if shape_count == 0 {
        eprintln!("Warning: no Shape node found — the output contains no geometry.");
    }

    if let Some(path) = &mtl_path {
        write_mtl(path, &materials)?;
    } else if !materials.is_empty() {
        eprintln!(
            "Note: writing to stdout, so no .mtl was produced. Materials referenced: {}",
            materials
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    eprintln!(
        "Converted {} shape{}: {} vertices, {} texture coordinates, {} normals",
        shape_count,
        if shape_count == 1 { "" } else { "s" },
        v_total,
        vt_total,
        vn_total
    );

    Ok(())
}

fn check_header(path: &str) -> Res<()> {
    let mut f = File::open(path).map_err(|e| format!("{}: {}", path, e))?;
    let mut head = [0u8; 5];
    match f.read(&mut head) {
        Ok(n) if n == 5 && &head == b"#VRML" => Ok(()),
        Ok(_) => {
            eprintln!("Warning: {} does not start with `#VRML`; parsing it anyway.", path);
            Ok(())
        }
        Err(e) => Err(format!("{}: {}", path, e).into()),
    }
}

fn current_node(stack: &[Frame]) -> Node {
    for f in stack.iter().rev() {
        if let Frame::Node(k) = f {
            return *k;
        }
    }
    Node::Other
}

/// `DEF SomeName Shape {` — the name two words back.
fn def_name(words: &[String]) -> Option<String> {
    if words.len() >= 3 && words[words.len() - 3] == "DEF" {
        Some(words[words.len() - 2].clone())
    } else {
        None
    }
}

fn apply_scalar_field(
    field: &str,
    vals: &[String],
    shape: &mut Option<Shape>,
    warned: &mut Warned,
) {
    if field == "translation" || field == "scale" {
        let identity = match field {
            "translation" => vals.iter().all(|v| v.parse::<f64>().map(|f| f == 0.0).unwrap_or(false)),
            _ => vals.iter().all(|v| v.parse::<f64>().map(|f| f == 1.0).unwrap_or(false)),
        };
        if !identity && !warned.transform {
            warned.transform = true;
            eprintln!(
                "Warning: Transform {} {} is not the identity and is NOT applied — \
                 the output keeps the raw coordinates.",
                field,
                vals.join(" ")
            );
        }
        return;
    }

    let Some(s) = shape.as_mut() else { return };
    let three = || -> Option<[String; 3]> {
        Some([vals.first()?.clone(), vals.get(1)?.clone(), vals.get(2)?.clone()])
    };
    match field {
        "diffuseColor" => s.material.diffuse = three(),
        "specularColor" => s.material.specular = three(),
        "shininess" => s.material.shininess = vals.first().cloned(),
        "transparency" => s.material.transparency = vals.first().cloned(),
        _ => {}
    }
}

/// Consume one data array, up to its closing `]`.
///
/// Coordinates are written straight to the OBJ as they stream past; index arrays are spooled to
/// disk because their siblings may not have arrived yet. Either way nothing accumulates in memory.
#[allow(clippy::too_many_arguments)]
fn read_array(
    kind: Array,
    lex: &mut Lexer,
    out: &mut Box<dyn Write>,
    shape: &mut Option<Shape>,
    v_total: &mut u64,
    vt_total: &mut u64,
    vn_total: &mut u64,
    tmp_dir: &std::path::Path,
    guard: &mut TmpGuard,
    warned: &mut Warned,
) -> Res<()> {
    let mut group: Vec<i64> = Vec::with_capacity(3);
    let arity = match kind {
        Array::Points | Array::Normals | Array::Colors => 3,
        Array::TexPoints => 2,
        _ => 0,
    };

    // Index arrays need a spool; create it lazily so an absent array stays absent.
    if matches!(kind, Array::CoordIndex | Array::TexCoordIndex | Array::NormalIndex) {
        if let Some(s) = shape.as_mut() {
            let (slot, suffix) = match kind {
                Array::CoordIndex => (&mut s.coord, "coord"),
                Array::TexCoordIndex => (&mut s.texcoord, "texcoord"),
                _ => (&mut s.normal, "normal"),
            };
            if slot.is_none() {
                let name = format!(
                    "objtools_import_{}_{}_{}.bin",
                    std::process::id(),
                    SPOOL_SEQ.fetch_add(1, Ordering::Relaxed),
                    suffix
                );
                *slot = Some(Spool::create(tmp_dir, &name, guard)?);
            }
        }
    }

    while let Some(tok) = lex.next_token()? {
        match tok {
            Tok::RBracket => break,
            Tok::Num => match kind {
                Array::Points | Array::TexPoints | Array::Normals => {
                    group.push(parse_decimal_fixed(&lex.text())?);
                    if group.len() == arity {
                        let Some(s) = shape.as_mut() else {
                            group.clear();
                            continue;
                        };
                        let prefix = match kind {
                            Array::Points => "v",
                            Array::TexPoints => "vt",
                            _ => "vn",
                        };
                        write!(out, "{}", prefix)?;
                        for g in &group {
                            write!(out, " {}", format_fixed_as_decimal(*g))?;
                        }
                        writeln!(out)?;
                        match kind {
                            Array::Points => {
                                s.v_count += 1;
                                *v_total += 1;
                            }
                            Array::TexPoints => {
                                s.vt_count += 1;
                                *vt_total += 1;
                            }
                            _ => {
                                s.vn_count += 1;
                                *vn_total += 1;
                            }
                        }
                        group.clear();
                    }
                }
                Array::Colors => {
                    if !warned.colors {
                        warned.colors = true;
                        eprintln!(
                            "Warning: per-vertex colours found; OBJ has no standard way to carry \
                             them, so they are dropped."
                        );
                    }
                }
                Array::CoordIndex | Array::TexCoordIndex | Array::NormalIndex => {
                    let idx: i64 = lex.text().parse()?;
                    let idx = i32::try_from(idx)
                        .map_err(|_| format!("index out of range for OBJ: {}", idx))?;
                    if let Some(s) = shape.as_mut() {
                        let slot = match kind {
                            Array::CoordIndex => &mut s.coord,
                            Array::TexCoordIndex => &mut s.texcoord,
                            _ => &mut s.normal,
                        };
                        if let Some(sp) = slot.as_mut() {
                            sp.push(idx)?;
                        }
                    }
                }
                Array::Urls | Array::Skip => {}
            },
            Tok::Str => {
                if kind == Array::Urls {
                    if let Some(s) = shape.as_mut() {
                        if s.material.texture.is_none() {
                            s.material.texture = Some(lex.text());
                        }
                    }
                }
            }
            // A nested brace inside a data array is not something we know how to read.
            Tok::LBrace | Tok::LBracket => {
                return Err("unexpected nested node inside a data array".into())
            }
            _ => {}
        }
    }

    if arity > 0 && !group.is_empty() && !warned.ragged {
        warned.ragged = true;
        eprintln!(
            "Warning: a coordinate array held a partial tuple ({} leftover value(s)); ignored.",
            group.len()
        );
    }

    Ok(())
}

/// Replay the spooled index arrays in lockstep and write the `f` lines.
fn finish_shape(
    mut s: Shape,
    out: &mut Box<dyn Write>,
    materials: &mut Vec<Material>,
    guard: &mut TmpGuard,
    warned: &mut Warned,
) -> Res<()> {
    if !s.material.is_empty() {
        s.material.name = format!("{}_mtl", s.name);
        writeln!(out, "usemtl {}", s.material.name)?;
        materials.push(std::mem::take(&mut s.material));
    }

    for sp in [&mut s.coord, &mut s.texcoord, &mut s.normal].into_iter().flatten() {
        sp.finish()?;
    }

    let Some(coord) = s.coord.as_ref() else {
        return Ok(());
    };

    let mut cr = BufReader::with_capacity(256 * 1024, File::open(&coord.path)?);
    let mut tr = match s.texcoord.as_ref() {
        Some(sp) => Some(BufReader::with_capacity(256 * 1024, File::open(&sp.path)?)),
        None => None,
    };
    let mut nr = match s.normal.as_ref() {
        Some(sp) => Some(BufReader::with_capacity(256 * 1024, File::open(&sp.path)?)),
        None => None,
    };

    // VRML: an absent texCoordIndex/normalIndex means "reuse coordIndex".
    let tex_from_coord = tr.is_none() && s.vt_count > 0;
    let normal_from_coord = nr.is_none() && s.vn_count > 0;

    let mut cf: Vec<i32> = Vec::new();
    let mut tf: Vec<i32> = Vec::new();
    let mut nf: Vec<i32> = Vec::new();

    while read_face(&mut cr, &mut cf)? {
        if let Some(r) = tr.as_mut() {
            if !read_face(r, &mut tf)? {
                tf.clear();
            }
        }
        if let Some(r) = nr.as_mut() {
            if !read_face(r, &mut nf)? {
                nf.clear();
            }
        }

        if cf.len() < 3 {
            if !warned.short_face {
                warned.short_face = true;
                eprintln!("Warning: face with fewer than 3 vertices skipped.");
            }
            continue;
        }

        write!(out, "f")?;
        for (i, ci) in cf.iter().enumerate() {
            let v = *ci as u64 + s.v_base + 1;
            let vt = if tex_from_coord {
                Some(*ci as u64 + s.vt_base + 1)
            } else {
                tf.get(i).map(|t| *t as u64 + s.vt_base + 1)
            };
            let vn = if normal_from_coord {
                Some(*ci as u64 + s.vn_base + 1)
            } else {
                nf.get(i).map(|n| *n as u64 + s.vn_base + 1)
            };
            match (vt, vn) {
                (None, None) => write!(out, " {}", v)?,
                (Some(t), None) => write!(out, " {}/{}", v, t)?,
                (None, Some(n)) => write!(out, " {}//{}", v, n)?,
                (Some(t), Some(n)) => write!(out, " {}/{}/{}", v, t, n)?,
            }
        }
        writeln!(out)?;
    }

    for sp in [&s.coord, &s.texcoord, &s.normal].into_iter().flatten() {
        guard.release(&sp.path);
    }

    Ok(())
}

/// VRML happily writes `.1`; some MTL readers only accept `0.1`.
fn num(raw: &str) -> String {
    match raw.parse::<f64>() {
        Ok(v) => format!("{}", v),
        Err(_) => raw.to_string(),
    }
}

fn write_mtl(path: &std::path::Path, materials: &[Material]) -> Res<()> {
    let mut f = BufWriter::new(File::create(path).map_err(|e| format!("{}: {}", path.display(), e))?);
    writeln!(f, "# Generated by objtools")?;
    for m in materials {
        writeln!(f)?;
        writeln!(f, "newmtl {}", m.name)?;
        if let Some(d) = &m.diffuse {
            writeln!(f, "Kd {} {} {}", num(&d[0]), num(&d[1]), num(&d[2]))?;
        }
        if let Some(s) = &m.specular {
            writeln!(f, "Ks {} {} {}", num(&s[0]), num(&s[1]), num(&s[2]))?;
        }
        // VRML shininess is 0..1; OBJ Ns is 0..1000.
        if let Some(sh) = &m.shininess {
            if let Ok(v) = sh.parse::<f64>() {
                writeln!(f, "Ns {}", v * 1000.0)?;
            }
        }
        if let Some(t) = &m.transparency {
            if let Ok(v) = t.parse::<f64>() {
                writeln!(f, "d {}", 1.0 - v)?;
            }
        }
        if let Some(tex) = &m.texture {
            writeln!(f, "map_Kd {}", tex)?;
        }
    }
    f.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_path(suffix: &str) -> PathBuf {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let mut p = std::env::temp_dir();
        p.push(format!("objtools_vrml_test_{}_{}", now, suffix));
        p
    }

    /// Write `src` to a .wrl, convert it, and hand back the .obj text.
    fn convert_str(src: &str) -> String {
        let input = tmp_path("in.wrl");
        let output = tmp_path("out.obj");
        std::fs::write(&input, src).unwrap();

        convert(&ImportOptions {
            file_path: input.to_string_lossy().into_owned(),
            output: Some(output.to_string_lossy().into_owned()),
            tmp_dir: std::env::temp_dir(),
            keep_tmp: false,
            progress: false,
        })
        .expect("convert");

        let s = std::fs::read_to_string(&output).unwrap();
        let _ = std::fs::remove_file(&input);
        let _ = std::fs::remove_file(&output);
        let _ = std::fs::remove_file(output.with_extension("mtl"));
        s
    }

    fn lines_starting(obj: &str, prefix: &str) -> Vec<String> {
        obj.lines()
            .filter(|l| l.starts_with(prefix))
            .map(|l| l.to_string())
            .collect()
    }

    #[test]
    fn pairs_coord_and_texcoord_indices_declared_out_of_order() {
        // texCoordIndex before coordIndex — the case the real-world files hit, and the reason
        // the index arrays are spooled instead of written on sight.
        let obj = convert_str(
            r#"#VRML V2.0 utf8
Shape {
  geometry IndexedFaceSet {
    coord Coordinate { point [ 0 0 0, 1 0 0, 1 1 0 ] }
    texCoord TextureCoordinate { point [ 0 0, 1 0, 1 1 ] }
    texCoordIndex [ 0, 1, 2, -1 ]
    coordIndex [ 2, 0, 1, -1 ]
  }
}"#,
        );
        assert_eq!(lines_starting(&obj, "f "), vec!["f 3/1 1/2 2/3"]);
    }

    #[test]
    fn absent_texcoordindex_falls_back_to_coordindex() {
        let obj = convert_str(
            r#"#VRML V2.0 utf8
Shape {
  geometry IndexedFaceSet {
    coord Coordinate { point [ 0 0 0, 1 0 0, 1 1 0 ] }
    texCoord TextureCoordinate { point [ 0 0, 1 0, 1 1 ] }
    coordIndex [ 0, 1, 2, -1 ]
  }
}"#,
        );
        assert_eq!(lines_starting(&obj, "f "), vec!["f 1/1 2/2 3/3"]);
    }

    #[test]
    fn several_shapes_get_offset_indices_and_their_def_names() {
        let obj = convert_str(
            r#"#VRML V2.0 utf8
Group { children [
  Shape { geometry IndexedFaceSet {
    coord Coordinate { point [ 0 0 0, 1 0 0, 0 1 0 ] }
    coordIndex [ 0, 1, 2, -1 ] } }
  DEF Second Shape { geometry IndexedFaceSet {
    coord Coordinate { point [ 5 5 5, 6 5 5, 5 6 5, 6 6 5 ] }
    normal Normal { vector [ 0 0 1, 0 0 1, 0 0 1, 0 0 1 ] }
    coordIndex [ 0, 1, 3, 2, -1 ] } }
] }"#,
        );
        assert_eq!(lines_starting(&obj, "o "), vec!["o shape_1", "o Second"]);
        // second shape's vertices start at 4; the n-gon is preserved, not triangulated
        assert_eq!(
            lines_starting(&obj, "f "),
            vec!["f 1 2 3", "f 4//1 5//2 7//4 6//3"]
        );
    }

    #[test]
    fn georeferenced_coordinates_survive_untouched() {
        let obj = convert_str(
            r#"#VRML V2.0 utf8
Shape { geometry IndexedFaceSet {
  coord Coordinate { point [
    606190.550817 6403213.130458 315.845471,
    606190.551000 6403213.130581 315.845633,
    606190.550913 6403213.130685 315.845299 ] }
  coordIndex [ 0, 1, 2, -1 ] } }"#,
        );
        assert_eq!(
            lines_starting(&obj, "v "),
            vec![
                // note 606190.551000 loses only its trailing zeros, never a significant digit
                "v 606190.550817 6403213.130458 315.845471",
                "v 606190.551 6403213.130581 315.845633",
                "v 606190.550913 6403213.130685 315.845299",
            ]
        );
    }

    #[test]
    fn viewpoints_and_unknown_nodes_are_skipped() {
        let obj = convert_str(
            r#"#VRML V2.0 utf8
NavigationInfo { type [ "EXAMINE", "ANY" ] }
Transform {
  scale 1.0 1.0 1.0
  translation 0.0 0.0 0.0
  children [
    Viewpoint { position 1 2 3 description "cam.jpg" fieldOfView 0.17 }
    Shape { geometry IndexedFaceSet {
      creaseAngle .5
      solid FALSE
      coord Coordinate { point [ 0 0 0, 1 0 0, 0 1 0 ] }
      coordIndex [ 0, 1, 2, -1 ] } }
  ]
}"#,
        );
        // the Viewpoint's `position 1 2 3` must not be mistaken for geometry
        assert_eq!(lines_starting(&obj, "v ").len(), 3);
        assert_eq!(lines_starting(&obj, "f "), vec!["f 1 2 3"]);
    }

    #[test]
    fn material_and_texture_reach_the_usemtl_line() {
        let obj = convert_str(
            r#"#VRML V2.0 utf8
DEF Panel Shape {
  geometry IndexedFaceSet {
    coord Coordinate { point [ 0 0 0, 1 0 0, 0 1 0 ] }
    coordIndex [ 0, 1, 2, -1 ] }
  appearance Appearance {
    material Material { diffuseColor 0.34 0.56 0.87 specularColor .1 .1 .1 shininess .5 }
    texture ImageTexture { url "panel.jpg" } } }"#,
        );
        assert_eq!(lines_starting(&obj, "usemtl "), vec!["usemtl Panel_mtl"]);
        // usemtl must precede the faces it applies to
        let u = obj.lines().position(|l| l.starts_with("usemtl ")).unwrap();
        let f = obj.lines().position(|l| l.starts_with("f ")).unwrap();
        assert!(u < f, "usemtl at {} should come before the first face at {}", u, f);
    }

    #[test]
    fn empty_geometry_produces_no_faces() {
        let obj = convert_str("#VRML V2.0 utf8\nNavigationInfo { type [ \"ANY\" ] }\n");
        assert!(lines_starting(&obj, "f ").is_empty());
        assert!(lines_starting(&obj, "v ").is_empty());
    }
}
