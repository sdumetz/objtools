//! Area-weighted normal accumulation by source position index, backed by disk.
use crate::{
    fixed::SCALE,
    import::Res,
    obj::{self, Scene},
    triangulate,
};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{BufReader, Seek, SeekFrom, Write},
    path::Path,
};
const MAX_PENDING: usize = 65536;

fn flush(pending: &mut HashMap<u64, [f64; 3]>, sums: &mut File, count: u64) -> Res<()> {
    for (index, contribution) in pending.drain() {
        let mut sum = obj::fetch::<3>(sums, index, count)?.map(f64::from_le_bytes);
        for i in 0..3 {
            sum[i] += contribution[i];
        }
        sums.seek(SeekFrom::Start((index - 1) * 24))?;
        for value in sum {
            sums.write_all(&value.to_le_bytes())?;
        }
    }
    Ok(())
}

pub fn generate(scene: &Scene, dir: &Path, progress: bool) -> Res<File> {
    let mut sums = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(dir.join("smooth-normals"))?;
    sums.set_len(
        scene.counts[0]
            .checked_mul(24)
            .ok_or("Normal spool size overflow")?,
    )?;
    let mut positions = File::open(dir.join("v"))?;
    let mut faces = BufReader::new(File::open(dir.join("faces"))?);
    let mut pending: HashMap<u64, [f64; 3]> = HashMap::new();
    let mut count = 0u64;
    while let Some((_, _, corners)) = obj::read_face(&mut faces)? {
        count += 1;
        let anchor =
            obj::fetch::<3>(&mut positions, corners[0].v, scene.counts[0])?.map(i64::from_le_bytes);
        let mut points = Vec::with_capacity(corners.len());
        for corner in &corners {
            let raw =
                obj::fetch::<3>(&mut positions, corner.v, scene.counts[0])?.map(i64::from_le_bytes);
            points.push(std::array::from_fn(|i| {
                (raw[i] as i128 - anchor[i] as i128) as f64 / SCALE as f64
            }));
        }
        if triangulate::is_degenerate_triangle(&points) {
            continue;
        }
        let (triangles, _) =
            triangulate::triangulate(&points).map_err(|e| format!("Face {count}: {e}"))?;
        for [a, b, c] in triangles {
            let u: [f64; 3] = std::array::from_fn(|i| points[b][i] - points[a][i]);
            let v: [f64; 3] = std::array::from_fn(|i| points[c][i] - points[a][i]);
            let normal = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            for i in [a, b, c] {
                let index = corners[i].v;
                if pending.len() == MAX_PENDING && !pending.contains_key(&index) {
                    flush(&mut pending, &mut sums, scene.counts[0])?;
                }
                let sum = pending.entry(index).or_insert([0.; 3]);
                for axis in 0..3 {
                    sum[axis] += normal[axis];
                }
            }
        }
        if progress && count.is_multiple_of(1_000_000) {
            eprintln!("Smoothing: accumulated {count} faces");
        }
    }
    flush(&mut pending, &mut sums, scene.counts[0])?;
    Ok(sums)
}
