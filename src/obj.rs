//! OBJ decoding to random-access attribute spools and a sequential face spool.
use crate::{
    fixed::{parse_three_fixed, BBox},
    import::Res,
    material::{self, Material},
};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
};
pub const MAX_FACE: usize = 4096;
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct Corner {
    pub v: u64,
    pub vt: u64,
    pub vn: u64,
}
pub struct Scene {
    pub bbox: BBox,
    pub missing_normals: bool,
    pub objects: Vec<String>,
    pub materials: Vec<Material>,
    pub counts: [u64; 3],
}
fn index(raw: &str, count: u64) -> Res<u64> {
    let n: i64 = raw.parse()?;
    let n = if n < 0 {
        count as i128 + n as i128 + 1
    } else {
        n as i128
    };
    if n <= 0 || n > u64::MAX as i128 {
        return Err("OBJ index is zero or out of range".into());
    }
    Ok(n as u64)
}
fn corner(token: &str, counts: [u64; 3]) -> Res<Corner> {
    let mut parts = token.split('/');
    let v = index(parts.next().ok_or("Missing vertex index")?, counts[0])?;
    let vt = match parts.next() {
        Some(s) if !s.is_empty() => index(s, counts[1])?,
        _ => 0,
    };
    let vn = match parts.next() {
        Some(s) if !s.is_empty() => index(s, counts[2])?,
        _ => 0,
    };
    if parts.next().is_some() {
        return Err("Too many OBJ index components".into());
    }
    Ok(Corner { v, vt, vn })
}
fn floats(rest: &str, count: usize) -> Res<Vec<f64>> {
    let mut out = Vec::new();
    for s in rest.split_whitespace() {
        let n: f64 = s.parse()?;
        if !n.is_finite() {
            return Err("Non-finite attribute".into());
        }
        out.push(n);
    }
    if out.len() < count {
        return Err("Missing attribute components".into());
    }
    Ok(out)
}
pub fn spool(source: &Path, dir: &Path, progress: bool, texture_base: Option<&Path>) -> Res<Scene> {
    let mut attrs = [
        BufWriter::new(File::create(dir.join("v"))?),
        BufWriter::new(File::create(dir.join("vt"))?),
        BufWriter::new(File::create(dir.join("vn"))?),
    ];
    let mut faces = BufWriter::new(File::create(dir.join("faces"))?);
    let mut scene = Scene {
        bbox: BBox::new(),
        missing_normals: false,
        objects: vec!["default".into()],
        materials: vec![Material::new("default")],
        counts: [0; 3],
    };
    let mut definitions = HashMap::new();
    let mut names = HashMap::from([("default".to_owned(), 0u32)]);
    let mut object = 0u32;
    let mut material = 0u32;
    let mut warned = HashSet::new();
    let mut reader = BufReader::with_capacity(256 * 1024, File::open(source)?);
    let mut line = String::new();
    let mut line_no = 0;
    let mut bytes = 0;
    let mut next_progress = 100_000_000;
    // Bound records even for corrupt/pathological inputs, rather than allocate arbitrarily.
    loop {
        line.clear();
        let size = Read::by_ref(&mut reader)
            .take(1024 * 1024 + 1)
            .read_line(&mut line)?;
        if size == 0 {
            break;
        }
        if size > 1024 * 1024 {
            return Err("OBJ line exceeds 1 MiB limit".into());
        }
        bytes += size;
        line_no += 1;
        if progress && bytes >= next_progress {
            eprintln!("Spooling OBJ: {}", crate::format::fmt_bytes(bytes as u64));
            next_progress = bytes + 100_000_000;
        }
        let text = line.split('#').next().unwrap().trim();
        let (key, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let rest = rest.trim();
        let result: Res<()> = (|| {
            match key {
                "v" => {
                    let coords = parse_three_fixed(rest)?;
                    if rest.split_whitespace().count() != 3 {
                        return Err("Only XYZ positions are supported (homogeneous coordinates/vertex colors unsupported)".into());
                    }
                    scene.bbox.add(coords);
                    for n in coords {
                        attrs[0].write_all(&n.to_le_bytes())?;
                    }
                    scene.counts[0] += 1;
                }
                "vt" => {
                    let nums = floats(rest, 1)?;
                    if nums.len() > 3 {
                        return Err("Too many UV components".into());
                    }
                    if nums.get(2).is_some_and(|n| *n != 0.) {
                        return Err("3D texture coordinates are unsupported".into());
                    }
                    for n in [nums[0], *nums.get(1).unwrap_or(&0.)] {
                        attrs[1].write_all(&n.to_le_bytes())?;
                    }
                    scene.counts[1] += 1;
                }
                "vn" => {
                    let nums = floats(rest, 3)?;
                    if nums.len() != 3 {
                        return Err("Expected three normal components".into());
                    }
                    if nums.iter().all(|n| *n == 0.) {
                        scene.missing_normals = true;
                    }
                    for n in nums {
                        attrs[2].write_all(&n.to_le_bytes())?;
                    }
                    scene.counts[2] += 1;
                }
                "o" => {
                    object = u32::try_from(scene.objects.len())?;
                    scene.objects.push(if rest.is_empty() {
                        "unnamed".into()
                    } else {
                        rest.into()
                    });
                }
                "usemtl" => {
                    material = if let Some(id) = names.get(rest) {
                        *id
                    } else {
                        let id = u32::try_from(scene.materials.len())?;
                        scene.materials.push(Material::new(rest));
                        names.insert(rest.into(), id);
                        id
                    };
                }
                "mtllib" => {
                    let parent = source.parent().unwrap_or(Path::new("."));
                    if parent.join(rest).is_file() {
                        material::load(&parent.join(rest), texture_base, &mut definitions)?;
                    } else {
                        for name in rest.split_whitespace() {
                            material::load(&parent.join(name), texture_base, &mut definitions)?;
                        }
                    }
                }
                "f" => {
                    let mut corners = Vec::new();
                    for token in rest.split_whitespace() {
                        if corners.len() == MAX_FACE {
                            return Err(format!("Face exceeds {MAX_FACE} corners").into());
                        }
                        corners.push(corner(token, scene.counts)?);
                    }
                    if corners.len() < 3 {
                        return Err("Face has fewer than three corners".into());
                    }
                    scene.missing_normals |= corners.iter().any(|c| c.vn == 0);
                    faces.write_all(&object.to_le_bytes())?;
                    faces.write_all(&material.to_le_bytes())?;
                    faces.write_all(&(corners.len() as u32).to_le_bytes())?;
                    for c in corners {
                        for n in [c.v, c.vt, c.vn] {
                            faces.write_all(&n.to_le_bytes())?;
                        }
                    }
                }
                "" | "s" | "g" => {}
                _ => {
                    if warned.insert(key.to_owned()) {
                        eprintln!("Warning: unsupported OBJ record {key} is omitted");
                    }
                }
            }
            Ok(())
        })();
        result.map_err(|e| format!("{}:{line_no}: {e}", source.display()))?;
    }
    for writer in &mut attrs {
        writer.flush()?;
    }
    faces.flush()?;
    for m in &mut scene.materials {
        if let Some(def) = definitions.remove(&m.name) {
            *m = def;
        } else if m.name != "default" {
            eprintln!("Warning: undefined material {}; using default", m.name);
        }
    }
    Ok(scene)
}
pub fn read_face(reader: &mut impl Read) -> Res<Option<(usize, usize, Vec<Corner>)>> {
    let mut first = [0; 4];
    if reader.read(&mut first[..1])? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut first[1..])?;
    let object = u32::from_le_bytes(first) as usize;
    reader.read_exact(&mut first)?;
    let material = u32::from_le_bytes(first) as usize;
    reader.read_exact(&mut first)?;
    let n = u32::from_le_bytes(first) as usize;
    if !(3..=MAX_FACE).contains(&n) {
        return Err("Invalid face spool".into());
    }
    let mut corners = Vec::with_capacity(n);
    for _ in 0..n {
        let mut a = [0u64; 3];
        for x in &mut a {
            let mut b = [0; 8];
            reader.read_exact(&mut b)?;
            *x = u64::from_le_bytes(b);
        }
        corners.push(Corner {
            v: a[0],
            vt: a[1],
            vn: a[2],
        });
    }
    Ok(Some((object, material, corners)))
}
pub fn fetch<const N: usize>(file: &mut File, index: u64, count: u64) -> Res<[[u8; 8]; N]> {
    if index == 0 || index > count {
        return Err(format!("Index {index} outside attribute count {count}").into());
    }
    let offset = (index - 1)
        .checked_mul(N as u64 * 8)
        .ok_or("Attribute offset overflow")?;
    file.seek(SeekFrom::Start(offset))?;
    assert!(N <= 3, "Attribute records have at most three components");
    let mut record = [0; 24];
    file.read_exact(&mut record[..N * 8])?;
    let out = std::array::from_fn(|i| record[i * 8..i * 8 + 8].try_into().unwrap());
    Ok(out)
}
