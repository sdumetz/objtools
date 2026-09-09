use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::format::fmt_bytes;
use crate::tmp::TmpGuard;

pub struct SplitOptions {
    pub file_path: String,
    pub output_dir: PathBuf,
    pub by_material: bool,
    pub tmp_dir: PathBuf,
    pub keep_tmp: bool,
    pub progress: bool,
}

// ── name sanitisation ────────────────────────────────────────────────────────

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if " /\\:*?\"<>|".contains(c) { '_' } else { c })
        .collect()
}

fn unique_path(used: &mut HashSet<PathBuf>, base: &Path) -> PathBuf {
    if used.insert(base.to_path_buf()) {
        return base.to_path_buf();
    }
    let stem = base.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let ext = base.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let parent = base.parent().unwrap_or(Path::new(""));
    let mut i = 2usize;
    loop {
        let candidate = parent.join(format!("{}_{}{}", stem, i, ext));
        if used.insert(candidate.clone()) {
            return candidate;
        }
        i += 1;
    }
}

// ── face-file writer pool ────────────────────────────────────────────────────

const MAX_OPEN_FACE_FILES: usize = 400;

struct GroupMeta {
    face_path: PathBuf,
    writer: Option<BufWriter<File>>,
    materials: Vec<String>,          // ordered list of materials seen (for non-by-material mode)
    last_mat_name: String,           // to detect changes
}

impl GroupMeta {
    fn get_writer(&mut self) -> std::io::Result<&mut BufWriter<File>> {
        if self.writer.is_none() {
            let f = OpenOptions::new().create(true).append(true).open(&self.face_path)?;
            self.writer = Some(BufWriter::new(f));
        }
        Ok(self.writer.as_mut().unwrap())
    }

    fn close_writer(&mut self) -> std::io::Result<()> {
        if let Some(mut w) = self.writer.take() {
            w.flush()?;
        }
        Ok(())
    }
}

// ── pass 1 ───────────────────────────────────────────────────────────────────

pub fn run(opts: SplitOptions) -> Result<(), Box<dyn std::error::Error>> {
    let mut guard = TmpGuard::new(opts.keep_tmp);

    let verts_path   = guard.track(opts.tmp_dir.join("obj_split_verts.bin")).clone();
    let texcoords_path = guard.track(opts.tmp_dir.join("obj_split_texcoords.bin")).clone();
    let normals_path = guard.track(opts.tmp_dir.join("obj_split_normals.bin")).clone();

    let verts_file   = File::create(&verts_path)?;
    let texcoords_file = File::create(&texcoords_path)?;
    let normals_file = File::create(&normals_path)?;

    let mut verts_w   = BufWriter::new(verts_file);
    let mut texcoords_w = BufWriter::new(texcoords_file);
    let mut normals_w = BufWriter::new(normals_file);

    // group key = (obj_name, mat_name)
    // insertion-ordered map: use Vec + HashMap index
    let mut group_keys: Vec<(String, String)> = Vec::new();
    let mut group_index: HashMap<(String, String), usize> = HashMap::new();
    let mut groups: Vec<GroupMeta> = Vec::new();
    let mut open_count: usize = 0;

    let mut mtllib_lines: Vec<String> = Vec::new();
    let mut current_obj = String::from("(default)");
    let mut current_mat = String::from("(none)");
    let mut v_count:  u64 = 0;
    let mut vt_count: u64 = 0;
    let mut vn_count: u64 = 0;

    let mut bytes_read: u64 = 0;
    let mut next_progress: u64 = 100_000_000;

    let input = File::open(&opts.file_path)
        .map_err(|e| format!("{}: {}", opts.file_path, e))?;
    let reader = BufReader::with_capacity(64 * 1024, input);

    // helper: get-or-create group, returns index
    macro_rules! get_group {
        ($obj:expr, $mat:expr) => {{
            let key = ($obj.clone(), $mat.clone());
            if let Some(&i) = group_index.get(&key) {
                i
            } else {
                let slug_obj = sanitize(&$obj);
                let slug_mat = sanitize(&$mat);
                let fname = if opts.by_material {
                    format!("obj_split_faces__{}__{}.bin", slug_obj, slug_mat)
                } else {
                    format!("obj_split_faces__{}.bin", slug_obj)
                };
                let face_path_raw = opts.tmp_dir.join(&fname);
                let face_path = guard.track(face_path_raw).clone();
                // create/truncate
                File::create(&face_path)?;
                let idx = groups.len();
                groups.push(GroupMeta {
                    face_path,
                    writer: None,
                    materials: Vec::new(),
                    last_mat_name: String::new(),
                });
                group_keys.push(key.clone());
                group_index.insert(key, idx);
                idx
            }
        }};
    }

    for line_res in reader.lines() {
        let line = line_res?;
        bytes_read += line.len() as u64 + 1;

        if opts.progress && bytes_read >= next_progress {
            next_progress = bytes_read + 100_000_000;
            eprintln!("Pass 1: read {}", fmt_bytes(bytes_read));
        }

        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        match trimmed.split_once(' ') {
            Some(("mtllib", rest)) => {
                mtllib_lines.push(rest.trim().to_string());
            }
            Some(("o", rest)) => {
                current_obj = rest.trim().to_string();
                if !opts.by_material {
                    current_mat = String::from("(none)");
                }
            }
            Some(("usemtl", rest)) => {
                let mat = rest.trim().to_string();
                if opts.by_material {
                    current_mat = mat.clone();
                } else {
                    // write sentinel into current group's face file
                    let gi = get_group!(current_obj, current_mat);
                    let g = &mut groups[gi];
                    // ensure open
                    if open_count >= MAX_OPEN_FACE_FILES {
                        // close one that's open (just the current group if it's open)
                        if g.writer.is_some() {
                            g.close_writer()?;
                            open_count -= 1;
                        }
                    }
                    if g.writer.is_none() { open_count += 1; }
                    // find or create material index
                    let mat_idx = if let Some(pos) = g.materials.iter().position(|m| m == &mat) {
                        pos as u32
                    } else {
                        g.materials.push(mat.clone());
                        (g.materials.len() - 1) as u32
                    };
                    if g.last_mat_name != mat {
                        g.last_mat_name = mat.clone();
                        let w = g.get_writer()?;
                        // sentinel: N=0, then u32 mat_idx
                        w.write_all(&[0u8])?;
                        w.write_all(&mat_idx.to_le_bytes())?;
                    }
                }
            }
            Some(("v", rest)) => {
                let coords = parse_floats3(rest)?;
                verts_w.write_all(&coords[0].to_le_bytes())?;
                verts_w.write_all(&coords[1].to_le_bytes())?;
                verts_w.write_all(&coords[2].to_le_bytes())?;
                v_count += 1;
            }
            Some(("vt", rest)) => {
                let coords = parse_floats2(rest)?;
                texcoords_w.write_all(&coords[0].to_le_bytes())?;
                texcoords_w.write_all(&coords[1].to_le_bytes())?;
                vt_count += 1;
            }
            Some(("vn", rest)) => {
                let coords = parse_floats3(rest)?;
                normals_w.write_all(&coords[0].to_le_bytes())?;
                normals_w.write_all(&coords[1].to_le_bytes())?;
                normals_w.write_all(&coords[2].to_le_bytes())?;
                vn_count += 1;
            }
            Some(("f", rest)) => {
                let gi = get_group!(current_obj, current_mat);

                // manage open file count
                if open_count >= MAX_OPEN_FACE_FILES && groups[gi].writer.is_none() {
                    // close some writer that isn't gi
                    for g2 in groups.iter_mut() {
                        if g2.writer.is_some() {
                            g2.close_writer()?;
                            open_count -= 1;
                            break;
                        }
                    }
                }
                if groups[gi].writer.is_none() { open_count += 1; }

                let tokens: Vec<&str> = rest.split_whitespace().collect();
                let n = tokens.len() as u8;
                let w = groups[gi].get_writer()?;
                w.write_all(&[n])?;
                for tok in &tokens {
                    let (vi, vti, vni) = parse_face_vertex(tok, v_count, vt_count, vn_count)?;
                    w.write_all(&vi.to_le_bytes())?;
                    w.write_all(&vti.to_le_bytes())?;
                    w.write_all(&vni.to_le_bytes())?;
                }
            }
            _ => {}
        }
    }

    // flush all writers
    verts_w.flush()?;
    texcoords_w.flush()?;
    normals_w.flush()?;
    for g in groups.iter_mut() {
        g.close_writer()?;
    }

    if opts.progress {
        eprintln!("Pass 1 complete. {} groups to write.", groups.len());
    }

    // ── pass 2 ───────────────────────────────────────────────────────────────

    // re-open random-access readers
    let mut verts_r   = File::open(&verts_path)?;
    let mut texcoords_r = File::open(&texcoords_path)?;
    let mut normals_r = File::open(&normals_path)?;

    let mut used_paths: HashSet<PathBuf> = HashSet::new();

    for (gi, (obj_name, mat_name)) in group_keys.iter().enumerate() {
        if opts.progress {
            eprintln!("Pass 2: writing group {}/{}: {}", gi + 1, groups.len(), obj_name);
        }

        let g = &groups[gi];

        // sub-pass A: collect unique indices
        let mut v_indices:  Vec<u32> = Vec::new();
        let mut vt_indices: Vec<u32> = Vec::new();
        let mut vn_indices: Vec<u32> = Vec::new();

        {
            let face_f = File::open(&g.face_path)?;
            let mut face_r = BufReader::new(face_f);
            loop {
                let mut nbuf = [0u8; 1];
                match face_r.read_exact(&mut nbuf) {
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(e) => return Err(e.into()),
                }
                let n = nbuf[0];
                if n == 0 {
                    // sentinel: skip 4-byte mat index
                    let mut skip = [0u8; 4];
                    face_r.read_exact(&mut skip)?;
                    continue;
                }
                for _ in 0..n {
                    let mut rec = [0u8; 12];
                    face_r.read_exact(&mut rec)?;
                    let vi  = u32::from_le_bytes(rec[0..4].try_into().unwrap());
                    let vti = u32::from_le_bytes(rec[4..8].try_into().unwrap());
                    let vni = u32::from_le_bytes(rec[8..12].try_into().unwrap());
                    if vi  != 0 { v_indices.push(vi); }
                    if vti != 0 { vt_indices.push(vti); }
                    if vni != 0 { vn_indices.push(vni); }
                }
            }
        }

        v_indices.sort_unstable();  v_indices.dedup();
        vt_indices.sort_unstable(); vt_indices.dedup();
        vn_indices.sort_unstable(); vn_indices.dedup();

        // sub-pass B: write output .obj
        let slug_obj = sanitize(obj_name);
        let slug_mat = sanitize(mat_name);
        let out_name = if opts.by_material {
            format!("{}__{}.obj", slug_obj, slug_mat)
        } else {
            format!("{}.obj", slug_obj)
        };
        let out_base = opts.output_dir.join(&out_name);
        let out_path = unique_path(&mut used_paths, &out_base);

        let out_file = File::create(&out_path)?;
        let mut out = BufWriter::new(out_file);

        writeln!(out, "# Generated by objtools")?;
        for ml in &mtllib_lines {
            writeln!(out, "mtllib {}", ml)?;
        }
        writeln!(out, "o {}", obj_name)?;

        // vertices
        for &old_v in &v_indices {
            let offset = (old_v as u64 - 1) * 24;
            verts_r.seek(SeekFrom::Start(offset))?;
            let mut buf = [0u8; 24];
            verts_r.read_exact(&mut buf)?;
            let x = f64::from_le_bytes(buf[0..8].try_into().unwrap());
            let y = f64::from_le_bytes(buf[8..16].try_into().unwrap());
            let z = f64::from_le_bytes(buf[16..24].try_into().unwrap());
            writeln!(out, "v {} {} {}", x, y, z)?;
        }

        // texcoords
        for &old_vt in &vt_indices {
            let offset = (old_vt as u64 - 1) * 16;
            texcoords_r.seek(SeekFrom::Start(offset))?;
            let mut buf = [0u8; 16];
            texcoords_r.read_exact(&mut buf)?;
            let u = f64::from_le_bytes(buf[0..8].try_into().unwrap());
            let v = f64::from_le_bytes(buf[8..16].try_into().unwrap());
            writeln!(out, "vt {} {}", u, v)?;
        }

        // normals
        for &old_vn in &vn_indices {
            let offset = (old_vn as u64 - 1) * 24;
            normals_r.seek(SeekFrom::Start(offset))?;
            let mut buf = [0u8; 24];
            normals_r.read_exact(&mut buf)?;
            let x = f64::from_le_bytes(buf[0..8].try_into().unwrap());
            let y = f64::from_le_bytes(buf[8..16].try_into().unwrap());
            let z = f64::from_le_bytes(buf[16..24].try_into().unwrap());
            writeln!(out, "vn {} {} {}", x, y, z)?;
        }

        // faces (re-read)
        {
            let face_f = File::open(&g.face_path)?;
            let mut face_r = BufReader::new(face_f);
            // emit initial usemtl for by-material mode
            if opts.by_material && mat_name != "(none)" {
                writeln!(out, "usemtl {}", mat_name)?;
            }
            loop {
                let mut nbuf = [0u8; 1];
                match face_r.read_exact(&mut nbuf) {
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(e) => return Err(e.into()),
                }
                let n = nbuf[0];
                if n == 0 {
                    let mut ibuf = [0u8; 4];
                    face_r.read_exact(&mut ibuf)?;
                    let mat_idx = u32::from_le_bytes(ibuf) as usize;
                    if let Some(mname) = g.materials.get(mat_idx) {
                        writeln!(out, "usemtl {}", mname)?;
                    }
                    continue;
                }
                let mut verts_face: Vec<(u32, u32, u32)> = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let mut rec = [0u8; 12];
                    face_r.read_exact(&mut rec)?;
                    let vi  = u32::from_le_bytes(rec[0..4].try_into().unwrap());
                    let vti = u32::from_le_bytes(rec[4..8].try_into().unwrap());
                    let vni = u32::from_le_bytes(rec[8..12].try_into().unwrap());
                    verts_face.push((vi, vti, vni));
                }
                write!(out, "f")?;
                for (vi, vti, vni) in &verts_face {
                    let nv  = if *vi  != 0 { v_indices.binary_search(vi).unwrap()   as u32 + 1 } else { 0 };
                    let nvt = if *vti != 0 { vt_indices.binary_search(vti).unwrap() as u32 + 1 } else { 0 };
                    let nvn = if *vni != 0 { vn_indices.binary_search(vni).unwrap() as u32 + 1 } else { 0 };
                    match (nvt != 0, nvn != 0) {
                        (false, false) => write!(out, " {}", nv)?,
                        (true,  false) => write!(out, " {}/{}", nv, nvt)?,
                        (false, true)  => write!(out, " {}//{}",  nv, nvn)?,
                        (true,  true)  => write!(out, " {}/{}/{}", nv, nvt, nvn)?,
                    }
                }
                writeln!(out)?;
            }
        }

        out.flush()?;
    }

    if opts.progress {
        eprintln!("Done. {} file{} written.", groups.len(), if groups.len() == 1 { "" } else { "s" });
    }

    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn parse_floats3(s: &str) -> Result<[f64; 3], Box<dyn std::error::Error>> {
    let mut it = s.split_whitespace();
    let x: f64 = it.next().ok_or("missing x")?.parse()?;
    let y: f64 = it.next().ok_or("missing y")?.parse()?;
    let z: f64 = it.next().ok_or("missing z")?.parse()?;
    Ok([x, y, z])
}

fn parse_floats2(s: &str) -> Result<[f64; 2], Box<dyn std::error::Error>> {
    let mut it = s.split_whitespace();
    let u: f64 = it.next().ok_or("missing u")?.parse()?;
    let v: f64 = it.next().ok_or("missing v")?.parse()?;
    Ok([u, v])
}

/// Returns (v_idx, vt_idx, vn_idx) — 1-based, 0 = absent.
/// Negative indices resolved against running counts.
fn parse_face_vertex(
    tok: &str,
    v_count: u64, vt_count: u64, vn_count: u64,
) -> Result<(u32, u32, u32), Box<dyn std::error::Error>> {
    let mut parts = tok.splitn(3, '/');
    let vi  = resolve_idx(parts.next().unwrap_or(""), v_count)?;
    let vti = resolve_idx(parts.next().unwrap_or(""), vt_count)?;
    let vni = resolve_idx(parts.next().unwrap_or(""), vn_count)?;
    Ok((vi, vti, vni))
}

fn resolve_idx(s: &str, count: u64) -> Result<u32, Box<dyn std::error::Error>> {
    if s.is_empty() { return Ok(0); }
    let i: i64 = s.parse()?;
    let resolved = if i < 0 { count as i64 + i + 1 } else { i };
    if resolved <= 0 { return Err(format!("invalid index {} (count={})", i, count).into()); }
    Ok(resolved as u32)
}
