//! Area-weighted normal accumulation by source position index, backed by a mapped disk spool.
use crate::{
    fixed::SCALE,
    import::Res,
    obj::{self, Scene},
    triangulate,
};
use memmap2::MmapMut;
use std::{
    fs::{File, OpenOptions},
    io::BufReader,
    path::Path,
};

pub fn generate(scene: &Scene, positions: &[u8], dir: &Path, progress: bool) -> Res<MmapMut> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(dir.join("smooth-normals"))?;
    file.set_len(
        scene.counts[0]
            .checked_mul(24)
            .ok_or("Normal spool size overflow")?,
    )?;
    // SAFETY: the spool was just created in the private workspace and is only accessed through
    // this mapping. The file is sparse: a full disk surfaces as SIGBUS rather than an error.
    let mut sums = unsafe { MmapMut::map_mut(&file)? };
    let mut faces = BufReader::new(File::open(dir.join("faces"))?);
    let mut count = 0u64;
    while let Some(obj::Face { flat, corners, .. }) = obj::read_face(&mut faces)? {
        count += 1;
        // A face under `s off` belongs to no smoothing group, so it weighs on no neighbour.
        if flat {
            continue;
        }
        let anchor =
            obj::fetch::<3>(positions, corners[0].v, scene.counts[0])?.map(i64::from_le_bytes);
        let mut points = Vec::with_capacity(corners.len());
        for corner in &corners {
            let raw =
                obj::fetch::<3>(positions, corner.v, scene.counts[0])?.map(i64::from_le_bytes);
            points.push(std::array::from_fn(|i| {
                (raw[i] as i128 - anchor[i] as i128) as f64 / SCALE as f64
            }));
        }
        // The same triangles as the geometry pass, so faces of zero area weigh nothing here either.
        let Some(polygon) = triangulate::triangulate(&points) else {
            continue;
        };
        for [a, b, c] in polygon.triangles {
            let u: [f64; 3] = std::array::from_fn(|i| points[b][i] - points[a][i]);
            let v: [f64; 3] = std::array::from_fn(|i| points[c][i] - points[a][i]);
            let normal = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            for i in [a, b, c] {
                // In range: every corner position was fetched from the same-length spool above.
                let offset = (corners[i].v - 1) as usize * 24;
                let (sum, _) = sums[offset..offset + 24].as_chunks_mut::<8>();
                for (axis, n) in sum.iter_mut().zip(normal) {
                    *axis = (f64::from_le_bytes(*axis) + n).to_le_bytes();
                }
            }
        }
        if progress && count.is_multiple_of(1_000_000) {
            eprintln!("Smoothing: accumulated {count} faces");
        }
    }
    Ok(sums)
}
