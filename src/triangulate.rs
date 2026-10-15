//! Polygon triangulation that never fails: every face with area becomes triangles, and only
//! faces of zero area are reported as degenerate (`None`). Real exports contain repeated
//! corners, keyhole polygons (a hole joined to the outline by a doubled bridge edge) and
//! self-intersecting faces. Aborting a long import on one of them helps nobody, and dropping
//! them leaves holes.
//!
//! Each rule is a separate step below, credited to the implementation it follows, so that any
//! one of them can be traced or swapped on its own:
//!
//! 1. [`clean`]: drop consecutive coincident corners (earcut).
//! 2. [`plane`]: projection plane and flat normal from Newell's normal, else from the largest
//!    fan triangle (ours).
//! 3. [`is_ear`]: the ear test of earcut.
//! 4. [`forced_tip`]: when no ear exists, Blender's "desperate mode", after Held's FIST.
//! 5. [`has_area`]: zero-area triangles are dropped, as zero-area input triangles are.
//!
//! "earcut" is mapbox/earcut 3.0.1 (`src/earcut.js`, ISC licence), which obj2gltf uses through
//! Cesium's `PolygonPipeline`. "Blender" is `source/blender/blenlib/intern/polyfill_2d.cc`.
//! No code is copied from either; only the rules are.

/// A triangulated face.
pub struct Polygon {
    /// Corner indices into the input points, each triangle in source winding order.
    pub triangles: Vec<[usize; 3]>,
    /// Unit normal of the face's plane, the flat normal for corners without a usable one.
    pub normal: [f32; 3],
    /// Whether step 4 had to force a clip: the face is self-intersecting or otherwise not a
    /// simple polygon, so its triangles may overlap or face backwards.
    pub forced: bool,
}

type Point = [f64; 3];

fn cross3(a: Point, b: Point, c: Point) -> Point {
    let u: Point = std::array::from_fn(|i| b[i] - a[i]);
    let v: Point = std::array::from_fn(|i| c[i] - a[i]);
    [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ]
}

fn length(n: Point) -> f64 {
    n.iter().map(|x| x * x).sum::<f64>().sqrt()
}

/// Step 5. Exact: a triangle is dropped only when its corners are collinear in 3D, not merely
/// in the projection, which a non-planar face can make look flat.
fn has_area(points: &[Point], [a, b, c]: [usize; 3]) -> bool {
    cross3(points[a], points[b], points[c])
        .iter()
        .any(|n| *n != 0.)
}

/// Step 1, earcut's `filterPoints` restricted to `equals(p, p.next)`: a corner at the same
/// position as the one before it spans no area, so dropping it loses nothing visible. This turns
/// `f 1 2 3 3`, a triangle written as a quad by some exporters, back into a triangle.
/// earcut also drops collinear corners, but only once ear search stalls. We keep them: in 3D
/// they may not be collinear at all, and steps 4–5 handle the rare stall they cause.
fn clean(points: &[Point]) -> Vec<usize> {
    let mut ring: Vec<usize> = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        if ring.last().is_none_or(|&j| points[j] != *p) {
            ring.push(i);
        }
    }
    while ring.len() > 1 && points[ring[0]] == points[*ring.last().unwrap()] {
        ring.pop();
    }
    ring
}

/// Step 2, our own rule. Newell's normal is the polygon's area vector and the textbook choice
/// for a possibly non-planar face. It vanishes when the face has no area, but also for a bowtie
/// whose two lobes cancel. The largest fan triangle then gives the plane, and only when that
/// vanishes too, which means that every corner lies on one line, is the face degenerate.
fn plane(points: &[Point], ring: &[usize]) -> Option<Point> {
    let mut newell = [0.; 3];
    for (k, &i) in ring.iter().enumerate() {
        let a = points[i];
        let b = points[ring[(k + 1) % ring.len()]];
        newell[0] += (a[1] - b[1]) * (a[2] + b[2]);
        newell[1] += (a[2] - b[2]) * (a[0] + b[0]);
        newell[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    let usable = |n: Point| length(n) > 0. && length(n).is_finite();
    if usable(newell) {
        return Some(newell);
    }
    ring.windows(2)
        .skip(1)
        .map(|w| cross3(points[ring[0]], points[w[0]], points[w[1]]))
        .filter(|n| usable(*n))
        .max_by(|a, b| length(*a).total_cmp(&length(*b)))
}

/// The face projected onto the coordinate plane its normal is most aligned with, and the
/// orientation that counts as convex there.
struct Projected {
    p: Vec<[f64; 2]>,
    sign: f64,
}

impl Projected {
    fn new(points: &[Point], ring: &[usize], normal: Point) -> Self {
        let drop = (0..3)
            .max_by(|a, b| normal[*a].abs().total_cmp(&normal[*b].abs()))
            .unwrap();
        let axes: Vec<_> = (0..3).filter(|a| *a != drop).collect();
        let p = points.iter().map(|p| [p[axes[0]], p[axes[1]]]).collect();
        let mut projected = Self { p, sign: 1. };
        let area: f64 = (1..ring.len() - 1)
            .map(|k| projected.cross(ring[0], ring[k], ring[k + 1]))
            .sum();
        // A bowtie's lobes cancel here as in 3D: orient by the triangle that gave the plane.
        let reference = if area != 0. {
            area
        } else {
            ring.windows(2)
                .skip(1)
                .map(|w| projected.cross(ring[0], w[0], w[1]))
                .max_by(|a, b| a.abs().total_cmp(&b.abs()))
                .unwrap_or(0.)
        };
        // Zero only through rounding; any orientation then works, with clips forced if need be.
        projected.sign = if reference < 0. { -1. } else { 1. };
        projected
    }
    /// Twice the signed area of triangle `abc`, positive when it turns like the face.
    fn cross(&self, a: usize, b: usize, c: usize) -> f64 {
        let p = &self.p;
        ((p[b][0] - p[a][0]) * (p[c][1] - p[a][1]) - (p[b][1] - p[a][1]) * (p[c][0] - p[a][0]))
            * self.sign
    }
    /// Inside or on the boundary of triangle `abc`, which must turn like the face.
    fn in_triangle(&self, [a, b, c]: [usize; 3], q: usize) -> bool {
        self.cross(a, b, q) >= 0. && self.cross(b, c, q) >= 0. && self.cross(c, a, q) >= 0.
    }
}

/// Step 3, earcut's `isEar`. The corner at `ring[k]` is an ear when it is strictly convex and no
/// other corner that is reflex or flat lies inside or on the candidate triangle. Only such corners
/// can block an ear of a simple polygon. As in earcut's `pointInTriangleExceptFirst`, a corner at
/// the same position as the triangle's first corner does not block: that is the doubled end of a
/// keyhole's bridge edge, which would otherwise block every ear touching it.
fn is_ear(pr: &Projected, ring: &[usize], k: usize) -> bool {
    let n = ring.len();
    let at = |j: usize| ring[j % n];
    let t = [at(k + n - 1), at(k), at(k + 1)];
    if pr.cross(t[0], t[1], t[2]) <= 0. {
        return false;
    }
    !(2..n - 1).any(|offset| {
        let j = k + offset;
        let q = at(j);
        pr.cross(at(j + n - 1), q, at(j + 1)) <= 0. && pr.p[q] != pr.p[t[0]] && pr.in_triangle(t, q)
    })
}

/// Step 4, Blender's "desperate mode" in `pf_ear_tip_find`, which credits Martin Held, "FIST:
/// Fast industrial-strength triangulation of polygons", Algorithmica 30 (2001). When no corner is
/// an ear, the face is not simple, or earlier clips made what remains degenerate. Clip a corner
/// that is convex or flat if there is one, otherwise the last one examined. Either way, clipping
/// always ends with n − 2 triangles that cover the face, overlapping where the face crosses itself.
/// earcut instead tries to cure local intersections and to split the polygon, then drops what is
/// left. obj2gltf inherits that, and it leaves a hole.
fn forced_tip(pr: &Projected, ring: &[usize]) -> usize {
    let n = ring.len();
    (0..n)
        .find(|&k| pr.cross(ring[(k + n - 1) % n], ring[k], ring[(k + 1) % n]) >= 0.)
        .unwrap_or(n - 1)
}

/// Triangulate a face given its corner positions, preferably relative to one of its corners
/// for precision. `None` means it has no area.
pub fn triangulate(points: &[Point]) -> Option<Polygon> {
    let mut ring = clean(points);
    if ring.len() < 3 {
        return None;
    }
    let normal = plane(points, &ring)?;
    let unit = normal.map(|n| (n / length(normal)) as f32);
    let pr = Projected::new(points, &ring, normal);
    let mut triangles = Vec::with_capacity(ring.len() - 2);
    let mut forced = false;
    while ring.len() > 3 {
        let n = ring.len();
        let k = (0..n).find(|&k| is_ear(&pr, &ring, k)).unwrap_or_else(|| {
            forced = true;
            forced_tip(&pr, &ring)
        });
        triangles.push([ring[(k + n - 1) % n], ring[k], ring[(k + 1) % n]]);
        ring.remove(k);
    }
    triangles.push([ring[0], ring[1], ring[2]]);
    triangles.retain(|t| has_area(points, *t));
    (!triangles.is_empty()).then_some(Polygon {
        triangles,
        normal: unit,
        forced,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn flat(xy: &[[f64; 2]]) -> Vec<Point> {
        xy.iter().map(|p| [p[0], p[1], 0.]).collect()
    }
    fn area(points: &[Point], t: [usize; 3]) -> f64 {
        cross3(points[t[0]], points[t[1]], points[t[2]])[2] / 2.
    }
    /// The polygon's own signed area, by the shoelace formula in the xy plane.
    fn shoelace(points: &[Point]) -> f64 {
        (0..points.len())
            .map(|i| {
                let (a, b) = (points[i], points[(i + 1) % points.len()]);
                a[0] * b[1] - b[0] * a[1]
            })
            .sum::<f64>()
            / 2.
    }
    /// A valid triangulation of a simple polygon: no forced clip, every triangle turning like
    /// the polygon, and areas adding up to the polygon's, so nothing overlaps or is missing.
    fn assert_exact(points: &[Point], expected: usize) {
        let polygon = triangulate(points).unwrap();
        assert!(!polygon.forced);
        assert_eq!(polygon.triangles.len(), expected);
        let total = shoelace(points);
        let mut sum = 0.;
        for t in &polygon.triangles {
            let a = area(points, *t);
            assert!(a * total > 0., "triangle {t:?} turns backwards");
            sum += a;
        }
        assert!(
            (sum - total).abs() <= 1e-9 * total.abs(),
            "{sum} != {total}"
        );
    }
    #[test]
    fn concave_both_windings() {
        let mut p = flat(&[[0., 0.], [2., 0.], [2., 2.], [1., 1.], [0., 2.]]);
        assert_exact(&p, 3);
        assert_eq!(triangulate(&p).unwrap().normal, [0., 0., 1.]);
        p.reverse();
        assert_exact(&p, 3);
        assert_eq!(triangulate(&p).unwrap().normal, [0., 0., -1.]);
    }
    #[test]
    fn repeated_corners_collapse() {
        // `f 1 2 3 3`, and a repeat that wraps around from the last corner to the first.
        let p = flat(&[[0., 0.], [1., 0.], [1., 1.], [1., 1.]]);
        assert_eq!(triangulate(&p).unwrap().triangles, vec![[0, 1, 2]]);
        let p = flat(&[[0., 0.], [1., 0.], [1., 1.], [0., 0.]]);
        assert_eq!(triangulate(&p).unwrap().triangles, vec![[0, 1, 2]]);
    }
    #[test]
    fn keyholes_in_both_windings() {
        // A 4×4 square with a 2×2 hole, joined at their lower left corners.
        let mut p = flat(&[
            [0., 0.],
            [4., 0.],
            [4., 4.],
            [0., 4.],
            [0., 0.],
            [1., 1.],
            [1., 3.],
            [3., 3.],
            [3., 1.],
            [1., 1.],
        ]);
        assert_exact(&p, 8);
        p.reverse();
        assert_exact(&p, 8);
    }
    #[test]
    fn zero_area_faces_are_degenerate() {
        assert!(triangulate(&[[0.; 3]; 3]).is_none());
        assert!(triangulate(&flat(&[[0., 0.], [1., 0.], [2., 0.], [3., 0.]])).is_none());
        assert!(triangulate(&flat(&[[0., 0.], [1., 1.], [0., 0.]])).is_none());
    }
    #[test]
    fn collinear_corners_keep_a_full_cover() {
        // A T-junction: the middle of the bottom edge is a corner of its own.
        assert_exact(
            &flat(&[[0., 0.], [1., 0.], [2., 0.], [2., 1.], [0., 1.]]),
            3,
        );
    }
    #[test]
    fn bowtie_takes_its_plane_from_a_fan_triangle() {
        // Lobes of equal area cancel in Newell's normal. Every corner still passes the ear test.
        let p = flat(&[[0., 0.], [1., 1.], [1., 0.], [0., 1.]]);
        let polygon = triangulate(&p).unwrap();
        assert_eq!(polygon.triangles.len(), 2);
        assert_eq!(polygon.normal.map(f32::abs), [0., 0., 1.]);
    }
    #[test]
    fn self_intersection_forces_clips() {
        // No corner of this crossed pentagon is an ear: step 4 forces one. The zero-area
        // triangle it leaves along the way is dropped.
        let p = flat(&[[1., 2.], [2., 2.], [1., 3.], [4., 0.], [0., 3.]]);
        let polygon = triangulate(&p).unwrap();
        assert!(polygon.forced);
        assert_eq!(polygon.triangles, vec![[0, 1, 3], [0, 3, 4]]);
    }
    /// Exact on integers: whether no two edges touch except adjacent ones at their shared
    /// corner, and no corner repeats.
    fn simple(p: &[[i64; 2]]) -> bool {
        let n = p.len();
        let orient = |a: [i64; 2], b: [i64; 2], c: [i64; 2]| {
            ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).signum()
        };
        let on = |a: [i64; 2], b: [i64; 2], c: [i64; 2]| {
            orient(a, b, c) == 0
                && c[0] >= a[0].min(b[0])
                && c[0] <= a[0].max(b[0])
                && c[1] >= a[1].min(b[1])
                && c[1] <= a[1].max(b[1])
        };
        let touch = |a, b, c, d| {
            let (o1, o2, o3, o4) = (
                orient(a, b, c),
                orient(a, b, d),
                orient(c, d, a),
                orient(c, d, b),
            );
            (o1 * o2 < 0 && o3 * o4 < 0) || on(a, b, c) || on(a, b, d) || on(c, d, a) || on(c, d, b)
        };
        for i in 0..n {
            for j in i + 1..n {
                if p[i] == p[j] {
                    return false;
                }
                let (a, b, c, d) = (p[i], p[(i + 1) % n], p[j], p[(j + 1) % n]);
                let adjacent = j == i + 1 || (i == 0 && j == n - 1);
                // Adjacent edges share one corner; they may only meet there, so neither may
                // contain the far end of the other.
                let bad = if adjacent {
                    let (shared, x, y) = if j == i + 1 { (b, a, d) } else { (a, b, c) };
                    on(shared, x, y) || on(shared, y, x)
                } else {
                    touch(a, b, c, d)
                };
                if bad {
                    return false;
                }
            }
        }
        true
    }
    #[test]
    fn random_simple_grid_polygons() {
        // Small integer grids make collinear corners, T-junctions and near-touching edges
        // common: every simple polygon among them must still triangulate exactly.
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut random = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n) as i64
        };
        let mut tested = 0;
        while tested < 5000 {
            let n = 4 + random(5) as usize;
            let grid: Vec<[i64; 2]> = (0..n).map(|_| [random(6), random(6)]).collect();
            if !simple(&grid) {
                continue;
            }
            tested += 1;
            let p: Vec<Point> = grid
                .iter()
                .map(|q| [q[0] as f64, q[1] as f64, 0.])
                .collect();
            assert_exact(&p, p.len() - 2);
        }
    }
    #[test]
    fn random_star_polygons_and_keyholes() {
        // Star-shaped polygons around the origin, and the same with a star-shaped hole bridged
        // along a ray through a vertex of both, are simple: they must triangulate exactly.
        // Jittered even angles keep every gap under π, so the origin is in the kernel. From six
        // corners, every outer edge also stays farther than 1 from it, which keeps the hole inside.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut random = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        for case in 0..2000 {
            let n = 3 + (random() * 12.) as usize;
            let angles: Vec<f64> = (0..n)
                .map(|i| (i as f64 + random() * 0.4) / n as f64 * std::f64::consts::TAU)
                .collect();
            let ring = |radius: &mut dyn FnMut() -> f64| -> Vec<[f64; 2]> {
                angles
                    .iter()
                    .map(|a| {
                        let r = radius();
                        [r * a.cos(), r * a.sin()]
                    })
                    .collect()
            };
            let outer = ring(&mut || 2. + random());
            let mut p = flat(&outer);
            if case % 2 == 1 && n >= 6 {
                // Same angles, so the bridge from corner 0 lies on a ray inside the annulus.
                let hole = ring(&mut || 0.5 + random() * 0.5);
                let mut keyhole = outer.clone();
                keyhole.push(outer[0]);
                keyhole.push(hole[0]);
                keyhole.extend(hole[1..].iter().rev());
                keyhole.push(hole[0]);
                p = flat(&keyhole);
            }
            let expected = p.len() - 2;
            assert_exact(&p, expected);
            p.reverse();
            assert_exact(&p, expected);
        }
    }
}
