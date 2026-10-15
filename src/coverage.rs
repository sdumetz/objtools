//! Texel coverage of UV triangles: one bit per texel in a sparse, memory-mapped disk file.
//! A texel is used when a triangle overlaps it (conservative rasterization), so texels shared by
//! several triangles count once and padding no triangle reaches is never counted.
use crate::import::Res;
use memmap2::MmapMut;
use std::{fs::OpenOptions, path::Path};
/// Texel coordinates beyond this are rejected rather than overflowing integer texel indices.
const LIMIT: f64 = (1u64 << 52) as f64;
/// A triangle repeating the texture more often than this vertically is left out: its rows would
/// be swept one by one. Horizontal repeats are cheap, since a row wider than two periods is full.
const MAX_REPEATS: i64 = 64;
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
/// Texel columns [floor(min x), ceil(max x)) of the triangle's part with y0 <= y <= y1.
fn span(tri: &[[f64; 2]; 3], y0: f64, y1: f64) -> Option<(i64, i64)> {
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
    Some((a, (hi.ceil() as i64).max(a + 1)))
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
    /// Mark the texels a UV triangle overlaps. Returns false when the triangle was left out
    /// because it repeats the texture more than `MAX_REPEATS` times vertically.
    pub fn triangle(&mut self, uv: [[f64; 2]; 3], wrap: [Wrap; 2]) -> Res<bool> {
        let (w, h) = (self.width, self.height);
        let tri = uv.map(|[u, v]| [u * w as f64, v * h as f64]);
        if tri.iter().flatten().any(|c| c.is_nan() || c.abs() >= LIMIT) {
            return Err("Texture coordinate is not finite or out of range".into());
        }
        let ymin = tri.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
        let ymax = tri.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max);
        let r0 = ymin.floor() as i64;
        let r1 = (ymax.ceil() as i64 - 1).max(r0);
        if wrap[1] == Wrap::Clamp {
            // Rows outside the texture all sample its edge rows.
            for t in r0.clamp(0, h - 1)..=r1.clamp(0, h - 1) {
                let y0 = if t == 0 { ymin } else { ymin.max(t as f64) };
                let y1 = if t == h - 1 {
                    ymax
                } else {
                    ymax.min((t + 1) as f64)
                };
                self.row(t, span(&tri, y0, y1), wrap[0]);
            }
        } else {
            if r1 - r0 >= MAX_REPEATS * h {
                return Ok(false);
            }
            for r in r0..=r1 {
                let s = span(&tri, ymin.max(r as f64), ymax.min((r + 1) as f64));
                self.row(wrap[1].index(r, h), s, wrap[0]);
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
            c.triangle([[0., 0.], [1., 0.], [1., 1.]], REPEAT).unwrap();
            c.triangle([[0., 0.], [1., 1.], [0., 1.]], REPEAT).unwrap();
        }
        assert_eq!(c.count(), 60);
    }
    #[test]
    fn texel_aligned_half_counts_diagonal_texels_once() {
        let (_t, mut c) = coverage(4, 4);
        c.triangle([[0., 0.], [1., 0.], [1., 1.]], REPEAT).unwrap();
        // 6 texels strictly below the diagonal, plus the 4 it crosses.
        assert_eq!(c.count(), 10);
    }
    #[test]
    fn tiny_triangle_uses_one_texel_and_edges_do_not_leak() {
        let (_t, mut c) = coverage(8, 8);
        c.triangle([[0.51, 0.51], [0.52, 0.51], [0.51, 0.52]], REPEAT)
            .unwrap();
        assert_eq!(c.count(), 1);
        // Exactly on texel boundaries: covers texels 2 and 3 of row 2 only.
        c.triangle([[0.25, 0.25], [0.5, 0.25], [0.25, 0.375]], REPEAT)
            .unwrap();
        assert_eq!(c.count(), 3);
    }
    #[test]
    fn wrap_modes() {
        // A strip one texel high, from u = -0.25 to 0.25 on an 8-texel-wide texture.
        let strip = |c: &mut Coverage, wrap: Wrap| {
            let tri = [[-0.25, 0.], [0.25, 0.], [0.25, 0.125]];
            c.triangle(tri, [wrap, Wrap::Repeat]).unwrap();
            c.triangle(
                [[-0.25, 0.], [0.25, 0.125], [-0.25, 0.125]],
                [wrap, Wrap::Repeat],
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
        c.triangle([[0., -3.], [0.1, -3.], [0., -2.]], [Wrap::Clamp; 2])
            .unwrap();
        assert_eq!(c.count(), 1);
        assert_eq!(c.map[0], 1);
    }
    /// Separating-axis test of a triangle against the open texel square [c, c+1] x [r, r+1].
    fn overlaps(tri: &[[f64; 2]; 3], c: f64, r: f64) -> bool {
        let square = [[c, r], [c + 1., r], [c + 1., r + 1.], [c, r + 1.]];
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
            let (_t, mut c) = coverage(w, h);
            c.triangle(tri, [Wrap::Clamp; 2]).unwrap();
            let texels = tri.map(|[x, y]| [x * w as f64, y * h as f64]);
            for r in 0..h {
                for col in 0..w {
                    let set = c.map[r as usize * c.stride + col as usize / 8] >> (col % 8) & 1 == 1;
                    assert_eq!(
                        set,
                        overlaps(&texels, col.into(), r.into()),
                        "case {case}: texel {col},{r} of {tri:?}"
                    );
                }
            }
        }
        assert!(tested > 5000, "{tested}");
    }
    #[test]
    fn tiling_triangles() {
        let (_t, mut c) = coverage(8, 8);
        c.triangle([[0., 0.], [100., 0.], [100., 1.]], REPEAT)
            .unwrap();
        assert_eq!(c.count(), 64);
        let (_t, mut c) = coverage(8, 8);
        let skipped = !c.triangle([[0., 0.], [1., 0.], [1., 65.]], REPEAT).unwrap();
        assert!(skipped);
        assert_eq!(c.count(), 0);
        assert!(c
            .triangle([[f64::NAN, 0.], [1., 0.], [1., 1.]], REPEAT)
            .is_err());
    }
}
