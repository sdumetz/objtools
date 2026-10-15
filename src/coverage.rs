//! Texel coverage of UV triangles: one bit per texel in a sparse, memory-mapped disk file.
//! A texel is used when a GPU sampling any point of a triangle reads it from the base level:
//! with nearest filtering the texel containing the point, with linear filtering the 2x2 texels
//! around it. Texels shared by several triangles count once, and padding no sample reaches is
//! never counted.
use crate::import::Res;
use memmap2::MmapMut;
use std::{fs::OpenOptions, path::Path};
/// Texel coordinates beyond this are rejected rather than overflowing integer texel indices.
const LIMIT: f64 = (1u64 << 52) as f64;
/// A triangle repeating the texture more often than this vertically is left out: its rows would
/// be swept one by one. Horizontal repeats are cheap, since a row wider than two periods is full.
const MAX_REPEATS: i64 = 64;
/// How the GPU reads the base level around a sample point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    /// The texel containing the point.
    Nearest,
    /// The 2x2 texels around it: every texel whose center is less than one texel away on each
    /// axis, since bilinear weights only vanish one full texel from a center.
    Linear,
}
impl Filter {
    /// From glTF `magFilter`/`minFilter`. The base level is read without blending only when
    /// both are nearest; engines default unset filters to linear.
    pub fn from_gl(mag: Option<u64>, min: Option<u64>) -> Self {
        // NEAREST, NEAREST_MIPMAP_NEAREST and NEAREST_MIPMAP_LINEAR sample each level nearest.
        if mag == Some(9728) && matches!(min, Some(9728 | 9984 | 9986)) {
            Self::Nearest
        } else {
            Self::Linear
        }
    }
    /// Extra texels read beyond the containing one, on the far side of each axis once the
    /// coordinates are shifted by half a texel.
    fn reach(self) -> i64 {
        match self {
            Self::Nearest => 0,
            Self::Linear => 1,
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wrap {
    Repeat,
    Clamp,
    Mirror,
}
impl Wrap {
    pub fn from_gl(mode: u64) -> Res<Self> {
        match mode {
            10497 => Ok(Self::Repeat),
            33071 => Ok(Self::Clamp),
            33648 => Ok(Self::Mirror),
            _ => Err(format!("Unknown sampler wrap mode {mode}").into()),
        }
    }
    fn index(self, i: i64, n: i64) -> i64 {
        match self {
            Self::Repeat => i.rem_euclid(n),
            Self::Clamp => i.clamp(0, n - 1),
            Self::Mirror => {
                let m = i.rem_euclid(2 * n);
                if m < n {
                    m
                } else {
                    2 * n - 1 - m
                }
            }
        }
    }
    /// Map the plane texel range [a, b) onto texture ranges within [0, n).
    fn ranges(self, a: i64, b: i64, n: i64, mut f: impl FnMut(i64, i64)) {
        match self {
            Self::Clamp => f(a.clamp(0, n - 1), b.clamp(1, n)),
            _ if b - a >= 2 * n => f(0, n),
            _ => {
                let mut x = a;
                while x < b {
                    let period = x.div_euclid(n);
                    let end = b.min((period + 1) * n);
                    let (s, e) = (x - period * n, end - period * n);
                    if self == Self::Mirror && period.rem_euclid(2) == 1 {
                        f(n - e, n - s);
                    } else {
                        f(s, e);
                    }
                    x = end;
                }
            }
        }
    }
}
/// Texel columns [floor(min x), ceil(max x) + reach) of the triangle's part with y0 <= y <= y1.
fn span(tri: &[[f64; 2]; 3], y0: f64, y1: f64, reach: i64) -> Option<(i64, i64)> {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for i in 0..3 {
        let (mut p, mut q) = (tri[i], tri[(i + 1) % 3]);
        if p[1] > q[1] {
            std::mem::swap(&mut p, &mut q);
        }
        if q[1] < y0 || p[1] > y1 {
            continue;
        }
        let (xa, xb) = if q[1] == p[1] {
            (p[0], q[0])
        } else {
            let at = |y: f64| p[0] + (y - p[1]) / (q[1] - p[1]) * (q[0] - p[0]);
            (at(p[1].max(y0)), at(q[1].min(y1)))
        };
        lo = lo.min(xa).min(xb);
        hi = hi.max(xa).max(xb);
    }
    if lo > hi {
        return None;
    }
    let a = lo.floor() as i64;
    Some((a, (hi.ceil() as i64 + reach).max(a + 1)))
}
pub struct Coverage {
    map: MmapMut,
    width: i64,
    height: i64,
    stride: usize,
}
impl Coverage {
    pub fn create(path: &Path, width: u32, height: u32) -> Res<Self> {
        let stride = (width as usize).div_ceil(8);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        file.set_len(stride as u64 * height as u64)?;
        // SAFETY: the file was just created in the private workspace and is only accessed
        // through this mapping. It is sparse: a full disk surfaces as SIGBUS, not an error.
        let map = unsafe { MmapMut::map_mut(&file)? };
        Ok(Self {
            map,
            width: width.into(),
            height: height.into(),
            stride,
        })
    }
    /// Mark the texels sampling a UV triangle reads. Returns false when the triangle was left
    /// out because it repeats the texture more than `MAX_REPEATS` times vertically.
    ///
    /// Texel `i` is read by linear filtering at `x` when `i - 0.5 < x < i + 1.5`. Shifting
    /// coordinates by half a texel turns that into `i - 1 < x' < i + 1`: the texels a nearest
    /// lookup of `x'` would touch, extended by one on the far side. That is the `reach`.
    pub fn triangle(&mut self, uv: [[f64; 2]; 3], wrap: [Wrap; 2], filter: Filter) -> Res<bool> {
        let (w, h) = (self.width, self.height);
        let reach = filter.reach();
        let shift = reach as f64 / 2.;
        let tri = uv.map(|[u, v]| [u * w as f64 - shift, v * h as f64 - shift]);
        if tri.iter().flatten().any(|c| c.is_nan() || c.abs() >= LIMIT) {
            return Err("Texture coordinate is not finite or out of range".into());
        }
        let ymin = tri.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
        let ymax = tri.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max);
        let r0 = ymin.floor() as i64;
        let r1 = (ymax.ceil() as i64 - 1 + reach).max(r0);
        // Row t reads the triangle's part with t - reach <= y <= t + 1.
        let slab = |t: i64| (ymin.max((t - reach) as f64), ymax.min((t + 1) as f64));
        if wrap[1] == Wrap::Clamp {
            // Rows outside the texture all sample its edge rows.
            for t in r0.clamp(0, h - 1)..=r1.clamp(0, h - 1) {
                let (mut y0, mut y1) = slab(t);
                if t == 0 {
                    y0 = ymin;
                }
                if t == h - 1 {
                    y1 = ymax;
                }
                self.row(t, span(&tri, y0, y1, reach), wrap[0]);
            }
        } else {
            if r1 - r0 >= MAX_REPEATS * h {
                return Ok(false);
            }
            for r in r0..=r1 {
                let (y0, y1) = slab(r);
                self.row(wrap[1].index(r, h), span(&tri, y0, y1, reach), wrap[0]);
            }
        }
        Ok(true)
    }
    fn row(&mut self, row: i64, span: Option<(i64, i64)>, wrap: Wrap) {
        let Some((a, b)) = span else { return };
        let base = row as usize * self.stride;
        let line = &mut self.map[base..base + self.stride];
        wrap.ranges(a, b, self.width, |a, b| {
            let (a, b) = (a as usize, b as usize);
            let (first, last) = (a / 8, (b - 1) / 8);
            let low = 0xffu8 << (a % 8);
            let high = 0xffu8 >> (7 - (b - 1) % 8);
            if first == last {
                line[first] |= low & high;
            } else {
                line[first] |= low;
                line[first + 1..last].fill(0xff);
                line[last] |= high;
            }
        });
    }
    /// Used texels. Padding bits past each row's width are never set.
    pub fn count(&self) -> u64 {
        self.map.iter().map(|b| u64::from(b.count_ones())).sum()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn coverage(width: u32, height: u32) -> (Temp, Coverage) {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "objtools-coverage-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let c = Coverage::create(&path, width, height).unwrap();
        (Temp(path), c)
    }
    const REPEAT: [Wrap; 2] = [Wrap::Repeat; 2];
    #[test]
    fn full_square_and_overlap_count_once() {
        let (_t, mut c) = coverage(10, 6);
        for _ in 0..2 {
            c.triangle([[0., 0.], [1., 0.], [1., 1.]], REPEAT, Filter::Nearest)
                .unwrap();
            c.triangle([[0., 0.], [1., 1.], [0., 1.]], REPEAT, Filter::Nearest)
                .unwrap();
        }
        assert_eq!(c.count(), 60);
    }
    #[test]
    fn texel_aligned_half_counts_diagonal_texels_once() {
        let (_t, mut c) = coverage(4, 4);
        c.triangle([[0., 0.], [1., 0.], [1., 1.]], REPEAT, Filter::Nearest)
            .unwrap();
        // 6 texels strictly below the diagonal, plus the 4 it crosses.
        assert_eq!(c.count(), 10);
    }
    #[test]
    fn tiny_triangle_uses_one_texel_and_edges_do_not_leak() {
        let (_t, mut c) = coverage(8, 8);
        c.triangle(
            [[0.51, 0.51], [0.52, 0.51], [0.51, 0.52]],
            REPEAT,
            Filter::Nearest,
        )
        .unwrap();
        assert_eq!(c.count(), 1);
        // Exactly on texel boundaries: covers texels 2 and 3 of row 2 only.
        c.triangle(
            [[0.25, 0.25], [0.5, 0.25], [0.25, 0.375]],
            REPEAT,
            Filter::Nearest,
        )
        .unwrap();
        assert_eq!(c.count(), 3);
    }
    #[test]
    fn wrap_modes() {
        // A strip one texel high, from u = -0.25 to 0.25 on an 8-texel-wide texture.
        let strip = |c: &mut Coverage, wrap: Wrap| {
            let tri = [[-0.25, 0.], [0.25, 0.], [0.25, 0.125]];
            c.triangle(tri, [wrap, Wrap::Repeat], Filter::Nearest)
                .unwrap();
            c.triangle(
                [[-0.25, 0.], [0.25, 0.125], [-0.25, 0.125]],
                [wrap, Wrap::Repeat],
                Filter::Nearest,
            )
            .unwrap();
        };
        let (_t, mut c) = coverage(8, 8);
        strip(&mut c, Wrap::Repeat);
        assert_eq!(c.map[0], 0b1100_0011);
        let (_t, mut c) = coverage(8, 8);
        strip(&mut c, Wrap::Mirror);
        assert_eq!(c.map[0], 0b0000_0011);
        let (_t, mut c) = coverage(8, 8);
        strip(&mut c, Wrap::Clamp);
        assert_eq!(c.map[0], 0b0000_0011);
        // Clamped rows: a triangle far below the texture lands on row 0.
        let (_t, mut c) = coverage(8, 8);
        c.triangle(
            [[0., -3.], [0.1, -3.], [0., -2.]],
            [Wrap::Clamp; 2],
            Filter::Nearest,
        )
        .unwrap();
        assert_eq!(c.count(), 1);
        assert_eq!(c.map[0], 1);
    }
    #[test]
    fn linear_filtering_reads_the_2x2_footprint() {
        // A tiny triangle at texel coordinates ~(4.1, 4.1): bilinear sampling there blends
        // texels 3 and 4 on each axis, whose centers 3.5 and 4.5 are within one texel.
        let (_t, mut c) = coverage(8, 8);
        let tri = [[0.51, 0.51], [0.52, 0.51], [0.51, 0.52]];
        c.triangle(tri, REPEAT, Filter::Linear).unwrap();
        assert_eq!(c.count(), 4);
        assert_eq!((c.map[3], c.map[4]), (0b0001_1000, 0b0001_1000));
        // Texel-aligned left half: linear reads one more column on each side, and with repeat
        // wrapping the column left of u = 0 is the texture's last one.
        let (_t, mut c) = coverage(8, 8);
        c.triangle([[0., 0.], [0.5, 0.], [0.5, 1.]], REPEAT, Filter::Linear)
            .unwrap();
        c.triangle([[0., 0.], [0.5, 1.], [0., 1.]], REPEAT, Filter::Linear)
            .unwrap();
        assert!(c.map.iter().all(|row| *row == 0b1001_1111));
        let (_t, mut c) = coverage(8, 8);
        c.triangle(
            [[0., 0.], [0.5, 0.], [0.5, 1.]],
            [Wrap::Clamp; 2],
            Filter::Linear,
        )
        .unwrap();
        c.triangle(
            [[0., 0.], [0.5, 1.], [0., 1.]],
            [Wrap::Clamp; 2],
            Filter::Linear,
        )
        .unwrap();
        assert!(c.map.iter().all(|row| *row == 0b0001_1111));
    }
    #[test]
    fn filters_follow_gltf_samplers() {
        assert_eq!(Filter::from_gl(None, None), Filter::Linear);
        assert_eq!(Filter::from_gl(Some(9728), None), Filter::Linear);
        assert_eq!(Filter::from_gl(Some(9728), Some(9729)), Filter::Linear);
        assert_eq!(Filter::from_gl(Some(9728), Some(9985)), Filter::Linear);
        assert_eq!(Filter::from_gl(Some(9728), Some(9986)), Filter::Nearest);
        assert_eq!(Filter::from_gl(Some(9729), Some(9728)), Filter::Linear);
    }
    /// Separating-axis test of a triangle against the open square [c - m, c + 1 + m] x
    /// [r - m, r + 1 + m]: the texel itself (m = 0) or its bilinear footprint (m = 0.5).
    fn overlaps(tri: &[[f64; 2]; 3], c: f64, r: f64, m: f64) -> bool {
        let (c0, c1, r0, r1) = (c - m, c + 1. + m, r - m, r + 1. + m);
        let square = [[c0, r0], [c1, r0], [c1, r1], [c0, r1]];
        let mut axes = vec![[1., 0.], [0., 1.]];
        for i in 0..3 {
            let (p, q) = (tri[i], tri[(i + 1) % 3]);
            axes.push([q[1] - p[1], p[0] - q[0]]);
        }
        axes.iter().all(|a| {
            let project = |points: &[[f64; 2]]| {
                let d = points.iter().map(|p| p[0] * a[0] + p[1] * a[1]);
                (
                    d.clone().fold(f64::INFINITY, f64::min),
                    d.fold(f64::NEG_INFINITY, f64::max),
                )
            };
            let ((t0, t1), (s0, s1)) = (project(tri), project(&square));
            t0 < s1 && s0 < t1
        })
    }
    #[test]
    fn matches_brute_force_overlap() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut random = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let (w, h) = (13u32, 11u32);
        let mut tested = 0;
        for case in 0..8000 {
            // Mostly small triangles, as in photogrammetry atlases, plus some large ones.
            let scale = if case % 4 == 0 { 1. } else { 0.15 };
            let (u, v) = (random(), random());
            let tri: [[f64; 2]; 3] = std::array::from_fn(|_| {
                [u + (random() - 0.5) * scale, v + (random() - 0.5) * scale]
            });
            // Clamping would make degenerate edge-hugging triangles, which the open-square test rejects.
            if tri.iter().flatten().any(|c| !(0. ..=1.).contains(c)) {
                continue;
            }
            tested += 1;
            // Inside [0, 1], clamping adds no texel: what it reads off the edge is the edge
            // texel, which the footprint already includes.
            for (filter, margin) in [(Filter::Nearest, 0.), (Filter::Linear, 0.5)] {
                let (_t, mut c) = coverage(w, h);
                c.triangle(tri, [Wrap::Clamp; 2], filter).unwrap();
                let texels = tri.map(|[x, y]| [x * w as f64, y * h as f64]);
                for r in 0..h {
                    for col in 0..w {
                        let bit = c.map[r as usize * c.stride + col as usize / 8] >> (col % 8);
                        assert_eq!(
                            bit & 1 == 1,
                            overlaps(&texels, col.into(), r.into(), margin),
                            "case {case}, {filter:?}: texel {col},{r} of {tri:?}"
                        );
                    }
                }
            }
        }
        assert!(tested > 5000, "{tested}");
    }
    #[test]
    fn tiling_triangles() {
        let (_t, mut c) = coverage(8, 8);
        c.triangle([[0., 0.], [100., 0.], [100., 1.]], REPEAT, Filter::Nearest)
            .unwrap();
        assert_eq!(c.count(), 64);
        let (_t, mut c) = coverage(8, 8);
        let skipped = !c
            .triangle([[0., 0.], [1., 0.], [1., 65.]], REPEAT, Filter::Nearest)
            .unwrap();
        assert!(skipped);
        assert_eq!(c.count(), 0);
        assert!(c
            .triangle(
                [[f64::NAN, 0.], [1., 0.], [1., 1.]],
                REPEAT,
                Filter::Nearest
            )
            .is_err());
    }
}
