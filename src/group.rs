//! Group the face spool by primitive key, (object, material, UV layout), on disk.
//! Objects are already contiguous, since every `o` starts a new one. Materials can interleave
//! inside an object; only then is the spool rewritten, by a counting sort into a mapped file.
//! File order is kept within each key.
use crate::{
    import::Res,
    obj::{self, Scene},
};
use memmap2::MmapMut;
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::BufReader,
    path::Path,
};
pub struct Group {
    pub corners: u64,
    bytes: u64,
}
/// Validate every face and return the keys in first-appearance order, with the spool's faces
/// rearranged so that each key's faces are contiguous and in that order.
pub fn group(scene: &Scene, dir: &Path, progress: bool) -> Res<Vec<Group>> {
    let path = dir.join("faces");
    let mut faces = BufReader::with_capacity(256 * 1024, File::open(&path)?);
    let mut groups: Vec<Group> = Vec::new();
    let mut keys = HashMap::new();
    let mut current = usize::MAX;
    let mut interleaved = false;
    let mut count = 0u64;
    while let Some((object, material, corners)) = obj::read_face(&mut faces)? {
        count += 1;
        let uv = corners[0].vt != 0;
        let normal = corners[0].vn != 0;
        if corners
            .iter()
            .any(|c| (c.vt != 0) != uv || (c.vn != 0) != normal)
        {
            return Err(format!("Face {count} mixes missing and present UVs/normals").into());
        }
        if scene.materials[material].texture.is_some() && !uv {
            return Err(format!("Face {count}: textured material requires UV coordinates").into());
        }
        let id = *keys.entry((object, material, uv)).or_insert_with(|| {
            groups.push(Group {
                corners: 0,
                bytes: 0,
            });
            groups.len() - 1
        });
        if id != current {
            interleaved |= groups[id].bytes > 0;
            current = id;
        }
        groups[id].corners += corners.len() as u64;
        groups[id].bytes += 12 + 24 * corners.len() as u64;
    }
    if !interleaved {
        return Ok(groups);
    }
    if progress {
        eprintln!("Grouping {count} faces by object and material");
    }
    let sorted_path = dir.join("faces-grouped");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&sorted_path)?;
    file.set_len(groups.iter().map(|g| g.bytes).sum())?;
    // SAFETY: the file was just created in the private workspace and is only accessed through
    // this mapping. Every byte is written before the mapping is dropped.
    let mut sorted = unsafe { MmapMut::map_mut(&file)? };
    let mut cursors = Vec::with_capacity(groups.len());
    let mut offset = 0;
    for g in &groups {
        cursors.push(usize::try_from(offset)?);
        offset += g.bytes;
    }
    let mut faces = BufReader::with_capacity(256 * 1024, File::open(&path)?);
    let mut record = Vec::new();
    while let Some((object, material, corners)) = obj::read_face(&mut faces)? {
        let id = keys[&(object, material, corners[0].vt != 0)];
        record.clear();
        obj::encode_face(&mut record, object, material, &corners);
        sorted[cursors[id]..cursors[id] + record.len()].copy_from_slice(&record);
        cursors[id] += record.len();
    }
    // No msync: the page cache already serves the replay, and eviction writes the pages back.
    drop(sorted);
    drop(file);
    drop(faces);
    fs::rename(&sorted_path, &path)?;
    Ok(groups)
}
