//! Conservative voxelization of a model's triangles into a grid sized by the resolution alone.
//! Each cell accumulates the area and area-weighted position of the surface crossing it, the
//! streaming idea of Lindstrom's out-of-core simplification (SIGGRAPH 2000) without its quadrics.
use super::model::Model;
use crate::shared::Res;
/// Parts smaller than this fraction of the largest one's area are noise at placeholder scale.
const MIN_PART_AREA: f64 = 0.005;
/// A far-off floater squeezes the model into a corner of the grid, and removing it leaves the
/// grid mostly empty. When the remaining parts span less than this share of the longest axis,
/// the model is voxelized again on a grid fitted to them.
const REFIT_BELOW: f64 = 0.9;
/// A floater far enough to fit the model inside one cell takes two passes to find the model's
/// real size, and one more to get its cell bounds tight.
const MAX_PASSES: usize = 4;
/// Each thread's grid costs memory, so small models don't use more threads than they need.
const FACES_PER_THREAD: u64 = 100_000;
/// Fixed-point scale of accumulated areas, in units of one cell's face. Integer sums don't depend
/// on their order, which lets tests compare thread counts exactly. A cell overflows at 2^31 layers
/// of surface; a face smaller than 2^-32 of a cell's face still weighs one unit.
const ONE: f64 = (1u64 << 32) as f64;

type V3 = [f64; 3];
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn longest(min: V3, max: V3) -> f64 {
    let e = sub(max, min);
    e[0].max(e[1]).max(e[2])
}
/// Triangle/box overlap by separating axes, Akenine-Möller 2001, for a box of half extent 0.5.
fn overlaps(center: V3, t: &[V3; 3]) -> bool {
    let v = [sub(t[0], center), sub(t[1], center), sub(t[2], center)];
    let [a, b, c] = v;
    for ((a, b), c) in a.into_iter().zip(b).zip(c) {
        if a.min(b).min(c) > 0.5 || a.max(b).max(c) < -0.5 {
            return false;
        }
    }
    let e = [sub(v[1], v[0]), sub(v[2], v[1]), sub(v[0], v[2])];
    let separated = |a: V3, projections: [f64; 3]| {
        let r = 0.5 * (a[0].abs() + a[1].abs() + a[2].abs());
        let [p, q, s] = projections;
        p.min(q).min(s) > r || p.max(q).max(s) < -r
    };
    let n = cross(e[0], e[1]);
    let d = dot(n, v[0]);
    if separated(n, [d, d, d]) {
        return false;
    }
    for edge in e {
        for unit in [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]] {
            let a = cross(unit, edge);
            if separated(a, [dot(a, v[0]), dot(a, v[1]), dot(a, v[2])]) {
                return false;
            }
        }
    }
    true
}
/// Closest point on a triangle, Ericson, Real-Time Collision Detection 5.1.5.
fn closest(p: V3, t: &[V3; 3]) -> V3 {
    let [a, b, c] = *t;
    let along = |from: V3, d: V3, s: f64| std::array::from_fn(|k| from[k] + d[k] * s);
    let (ab, ac, ap) = (sub(b, a), sub(c, a), sub(p, a));
    let (d1, d2) = (dot(ab, ap), dot(ac, ap));
    if d1 <= 0. && d2 <= 0. {
        return a;
    }
    let bp = sub(p, b);
    let (d3, d4) = (dot(ab, bp), dot(ac, bp));
    if d3 >= 0. && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0. && d1 >= 0. && d3 <= 0. {
        return along(a, ab, d1 / (d1 - d3));
    }
    let cp = sub(p, c);
    let (d5, d6) = (dot(ab, cp), dot(ac, cp));
    if d6 >= 0. && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0. && d2 >= 0. && d6 <= 0. {
        return along(a, ac, d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0. && d4 - d3 >= 0. && d5 - d6 >= 0. {
        return along(b, sub(c, b), (d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let den = 1. / (va + vb + vc);
    along(along(a, ab, vb * den), ac, vc * den)
}
#[derive(Clone, Copy, Default)]
pub struct Cell {
    /// Area-weighted position relative to the cell's lower corner, in cell units.
    sum: [i64; 3],
    area: i64,
}
impl Cell {
    pub fn occupied(&self) -> bool {
        self.area > 0
    }
    /// Mean surface position relative to the cell's lower corner, in cell units.
    pub fn mean(&self) -> V3 {
        self.sum.map(|s| s as f64 / self.area as f64)
    }
    fn deposit(&mut self, relative: V3, area: f64) {
        let weight = area * ONE;
        for (sum, r) in self.sum.iter_mut().zip(relative) {
            *sum += (r * weight) as i64;
        }
        self.area += (weight as i64).max(1);
    }
    fn merge(&mut self, other: &Self) {
        for (sum, s) in self.sum.iter_mut().zip(other.sum) {
            *sum += s;
        }
        self.area += other.area;
    }
}
/// Cubic cells over the bounds, with one empty layer all around for the exterior flood fill.
#[derive(Clone, Copy)]
pub struct Frame {
    pub dims: [usize; 3],
    pub origin: V3,
    pub cell: f64,
}
impl Frame {
    fn new(min: V3, max: V3, resolution: usize) -> Res<Self> {
        let size = longest(min, max);
        if size <= 0. || !size.is_finite() {
            return Err("Model has no extent to build a placeholder around".into());
        }
        let cell = size / resolution as f64;
        Ok(Self {
            dims: std::array::from_fn(|k| (((max[k] - min[k]) / cell).ceil() as usize).max(1) + 2),
            origin: std::array::from_fn(|k| min[k] - cell),
            cell,
        })
    }
    pub fn id(&self, x: usize, y: usize, z: usize) -> usize {
        x + self.dims[0] * (y + self.dims[1] * z)
    }
    pub fn len(&self) -> usize {
        self.dims.iter().product()
    }
    pub fn coords(&self, id: usize) -> [usize; 3] {
        let [nx, ny, _] = self.dims;
        [id % nx, (id / nx) % ny, id / (nx * ny)]
    }
    /// Calls `f` for the ids of `id`'s neighbours sharing a face, edge or corner, and itself.
    fn around(&self, id: usize, mut f: impl FnMut(usize)) {
        let c = self.coords(id);
        let range = |k: usize| c[k].saturating_sub(1)..=(c[k] + 1).min(self.dims[k] - 1);
        for z in range(2) {
            for y in range(1) {
                for x in range(0) {
                    f(self.id(x, y, z));
                }
            }
        }
    }
}
pub struct Grid {
    pub frame: Frame,
    pub cells: Vec<Cell>,
}
/// Adds one triangle, in grid coordinates, to the inner cells it overlaps.
fn add_spanning(frame: &Frame, cells: &mut [Cell], t: &[V3; 3], area: f64, hits: &mut Vec<usize>) {
    let (mut lo, mut hi) = ([0usize; 3], [0usize; 3]);
    for k in 0..3 {
        let min = t[0][k].min(t[1][k]).min(t[2][k]);
        let max = t[0][k].max(t[1][k]).max(t[2][k]);
        let top = frame.dims[k] - 2;
        // Refitted grids leave out whatever lies beyond them. The tolerance keeps faces lying
        // exactly on the bounds, whose coordinates may round just past the last inner cell.
        if max < 1. || min > (top + 1) as f64 + 1e-6 {
            return;
        }
        lo[k] = (min as usize).clamp(1, top);
        hi[k] = (max as usize).clamp(1, top);
    }
    hits.clear();
    for z in lo[2]..=hi[2] {
        for y in lo[1]..=hi[1] {
            for x in lo[0]..=hi[0] {
                let center = [x as f64 + 0.5, y as f64 + 0.5, z as f64 + 0.5];
                if overlaps(center, t) {
                    hits.push(frame.id(x, y, z));
                }
            }
        }
    }
    // Clamped onto a cell it only touches, a face on the bounds may miss by rounding.
    if hits.is_empty() {
        hits.push(frame.id(lo[0], lo[1], lo[2]));
    }
    let share = area / hits.len() as f64;
    for &id in hits.iter() {
        let c = frame.coords(id).map(|n| n as f64);
        let p = closest([c[0] + 0.5, c[1] + 0.5, c[2] + 0.5], t);
        cells[id].deposit(sub(p, c), share);
    }
}
fn accumulate(frame: &Frame, model: &Model, start: u64, end: u64) -> Result<Vec<Cell>, String> {
    let mut cells = vec![Cell::default(); frame.len()];
    let mut hits = Vec::new();
    let scale = 1. / frame.cell;
    let inner = frame.dims.map(|n| n - 1);
    model.triangles(start, end, |face| {
        let t: [V3; 3] = face.map(|p| {
            [
                (p[0] - frame.origin[0]) * scale,
                (p[1] - frame.origin[1]) * scale,
                (p[2] - frame.origin[2]) * scale,
            ]
        });
        let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
        let area = 0.5 * dot(n, n).sqrt();
        if area <= 0. || area.is_nan() {
            return;
        }
        // Nearly every face of a dense mesh sits inside one cell: comparing truncated corners
        // settles those without the separating-axis tests. Truncation is floor here, since a
        // negative coordinate saturates to the empty layer at 0.
        let cell = |p: V3| [p[0] as usize, p[1] as usize, p[2] as usize];
        let c = cell(t[0]);
        if c == cell(t[1]) && c == cell(t[2]) && (0..3).all(|k| c[k] >= 1 && c[k] < inner[k]) {
            let centroid =
                std::array::from_fn(|k| (t[0][k] + t[1][k] + t[2][k]) / 3. - c[k] as f64);
            cells[frame.id(c[0], c[1], c[2])].deposit(centroid, area);
        } else {
            add_spanning(frame, &mut cells, &t, area, &mut hits);
        }
    })?;
    Ok(cells)
}
fn rasterize(frame: &Frame, model: &Model, threads: usize) -> Res<Vec<Cell>> {
    let threads = threads.min((model.faces / FACES_PER_THREAD + 1) as usize) as u64;
    let parts: Vec<Result<Vec<Cell>, String>> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|i| {
                let (start, end) = (model.faces * i / threads, model.faces * (i + 1) / threads);
                s.spawn(move || accumulate(frame, model, start, end))
            })
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().expect("voxelization thread panicked"))
            .collect()
    });
    let mut merged: Option<Vec<Cell>> = None;
    for part in parts {
        let part = part?;
        match &mut merged {
            None => merged = Some(part),
            Some(total) => {
                for (a, b) in total.iter_mut().zip(&part) {
                    a.merge(b);
                }
            }
        }
    }
    Ok(merged.unwrap_or_else(|| vec![Cell::default(); frame.len()]))
}
/// Empties the cells of parts below `MIN_PART_AREA`, parts being 26-connected groups of cells.
/// Returns how many were dropped and the bounds of what remains, if anything does.
fn drop_small_parts(grid: &mut Grid) -> (usize, Option<(V3, V3)>) {
    let frame = grid.frame;
    let mut part = vec![u32::MAX; frame.len()];
    let mut areas: Vec<f64> = Vec::new();
    let mut stack = Vec::new();
    for seed in 0..frame.len() {
        if !grid.cells[seed].occupied() || part[seed] != u32::MAX {
            continue;
        }
        let label = areas.len() as u32;
        let mut area = 0.;
        part[seed] = label;
        stack.push(seed);
        while let Some(id) = stack.pop() {
            area += grid.cells[id].area as f64;
            frame.around(id, |n| {
                if grid.cells[n].occupied() && part[n] == u32::MAX {
                    part[n] = label;
                    stack.push(n);
                }
            });
        }
        areas.push(area);
    }
    let largest = areas.iter().copied().fold(0., f64::max);
    let keep: Vec<bool> = areas
        .iter()
        .map(|a| *a >= MIN_PART_AREA * largest)
        .collect();
    let mut bounds: Option<(V3, V3)> = None;
    for (id, label) in part.iter().enumerate() {
        if *label == u32::MAX {
            continue;
        }
        if !keep[*label as usize] {
            grid.cells[id] = Cell::default();
            continue;
        }
        let c = frame.coords(id);
        let (lo, hi): (V3, V3) = (
            std::array::from_fn(|k| frame.origin[k] + c[k] as f64 * frame.cell),
            std::array::from_fn(|k| frame.origin[k] + (c[k] + 1) as f64 * frame.cell),
        );
        let b = bounds.get_or_insert((lo, hi));
        for k in 0..3 {
            b.0[k] = b.0[k].min(lo[k]);
            b.1[k] = b.1[k].max(hi[k]);
        }
    }
    (keep.iter().filter(|k| !**k).count(), bounds)
}
/// Voxelizes `model`, refitting the grid if dropping floaters leaves much of it empty.
pub fn build(model: &Model, resolution: usize, threads: usize, progress: bool) -> Res<Grid> {
    let (mut min, mut max) = (model.min, model.max);
    // Parts are only told apart at the resolution of the pass that finds them, so a later pass
    // may count others: the largest count is the one known to be real.
    let mut dropped = 0;
    for pass in 1..=MAX_PASSES {
        let frame = Frame::new(min, max, resolution)?;
        if progress {
            eprintln!(
                "Voxelizing {} faces into {}x{}x{} cells",
                model.faces, frame.dims[0], frame.dims[1], frame.dims[2]
            );
        }
        if pass == 1 {
            warn_about_precision(&frame, min, max);
        }
        let mut grid = Grid {
            frame,
            cells: rasterize(&frame, model, threads)?,
        };
        let (count, bounds) = drop_small_parts(&mut grid);
        dropped = dropped.max(count);
        let (kept_min, kept_max) =
            bounds.ok_or("Model has no surface to build a placeholder around")?;
        if pass < MAX_PASSES && longest(kept_min, kept_max) < REFIT_BELOW * longest(min, max) {
            (min, max) = (kept_min, kept_max);
            continue;
        }
        if dropped > 0 {
            eprintln!("Warning: left out at least {dropped} small disconnected parts");
        }
        return Ok(grid);
    }
    unreachable!("the last pass always returns")
}
/// float32 positions far from the origin are already rounded in the glTF, and a grid finer than
/// that rounding shows it as steps. Only `import --center` avoids it.
fn warn_about_precision(frame: &Frame, min: V3, max: V3) {
    let largest = (0..3).fold(0f64, |a, k| a.max(min[k].abs()).max(max[k].abs()));
    if largest * f64::from(f32::EPSILON) * 2. > frame.cell {
        eprintln!("Warning: coordinates this far from the origin have lost detail to float32; import with --center");
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// A small deterministic generator, so that failures reproduce.
    fn triangles(count: usize) -> Vec<[V3; 3]> {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64 * 4. - 2.
        };
        (0..count)
            .map(|_| std::array::from_fn(|_| [next(), next(), next()]))
            .collect()
    }
    fn on_triangle(t: &[V3; 3], u: f64, v: f64) -> V3 {
        std::array::from_fn(|k| t[0][k] + (t[1][k] - t[0][k]) * u + (t[2][k] - t[0][k]) * v)
    }
    fn samples(t: &[V3; 3], mut f: impl FnMut(V3)) {
        const N: usize = 40;
        for i in 0..=N {
            for j in 0..=N - i {
                f(on_triangle(t, i as f64 / N as f64, j as f64 / N as f64));
            }
        }
    }
    #[test]
    fn overlap_never_misses_a_sampled_point() {
        for t in triangles(2000) {
            let mut inside = false;
            samples(&t, |p| inside |= p.iter().all(|c| c.abs() < 0.5));
            assert!(!inside || overlaps([0.; 3], &t), "{t:?}");
        }
    }
    #[test]
    fn overlap_is_rejected_far_away() {
        let t = [[3., 3., 3.], [4., 3., 3.], [3., 4., 3.]];
        assert!(!overlaps([0.; 3], &t));
        // Every axis-aligned test passes for this one; only the plane or edge axes separate it.
        let t = [[1.5, -2., 0.], [-2., 1.5, 0.], [1.5, 1.5, 0.]];
        assert!(!overlaps([-0.9, -0.9, 0.], &t));
        assert!(overlaps([1., 1., 0.], &t));
    }
    #[test]
    fn closest_point_beats_every_sample() {
        let point = [0.3, -0.2, 0.7];
        let distance = |p: V3| {
            let d = sub(p, point);
            dot(d, d)
        };
        for t in triangles(500) {
            let c = closest(point, &t);
            samples(&t, |p| assert!(distance(c) <= distance(p) + 1e-9, "{t:?}"));
        }
    }
    #[test]
    fn fixed_point_sums_keep_tiny_faces_and_their_position() {
        let mut cell = Cell::default();
        cell.deposit([0.25, 0.5, 0.75], 1e-12);
        assert!(cell.occupied());
        let mut cell = Cell::default();
        for _ in 0..1000 {
            cell.deposit([0.25, 0.5, 0.75], 1e-5);
        }
        for (mean, expected) in cell.mean().iter().zip([0.25, 0.5, 0.75]) {
            assert!((mean - expected).abs() < 1e-4, "{mean} {expected}");
        }
    }
    #[test]
    fn frame_is_cubic_and_padded() {
        let frame = Frame::new([0., 0., 0.], [10., 2.5, 0.], 4).unwrap();
        assert_eq!(frame.cell, 2.5);
        assert_eq!(frame.dims, [6, 3, 3]);
        assert_eq!(frame.origin, [-2.5, -2.5, -2.5]);
        assert_eq!(frame.coords(frame.id(5, 2, 1)), [5, 2, 1]);
        assert!(Frame::new([1.; 3], [1.; 3], 4).is_err());
    }
}
