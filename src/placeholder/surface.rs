//! The outer surface of a voxel grid: Surface Nets (Gibson 1998) over binary cells, with each
//! vertex at the mean surface position of the occupied cells around it rather than at a lattice
//! corner, so the result follows the model instead of showing voxel steps. Texture coordinates
//! come from a box projection, which keeps the checker's squares even on every side.
use super::voxel::{cross, Grid};
use crate::shared::Res;
/// Triangles below this area, in cell faces, are slivers from collapsed vertices.
const MIN_AREA: f64 = 1e-6;
/// Checker repeats along the model's longest axis, each one a dark and a light square.
const REPEATS: f64 = 8.;
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}
/// Cells the exterior reaches through shared faces, starting from the empty outer layer.
/// Enclosed cavities aren't reached, so interior geometry and hidden chambers produce no surface.
fn exterior(grid: &Grid) -> Vec<bool> {
    let frame = &grid.frame;
    let mut outside = vec![false; frame.len()];
    outside[0] = true;
    let mut stack = vec![0];
    while let Some(id) = stack.pop() {
        let [x, y, z] = frame.coords(id);
        let [nx, ny, nz] = frame.dims;
        let mut visit = |n: usize| {
            if !outside[n] && !grid.cells[n].occupied() {
                outside[n] = true;
                stack.push(n);
            }
        };
        if x > 0 {
            visit(frame.id(x - 1, y, z));
        }
        if x + 1 < nx {
            visit(frame.id(x + 1, y, z));
        }
        if y > 0 {
            visit(frame.id(x, y - 1, z));
        }
        if y + 1 < ny {
            visit(frame.id(x, y + 1, z));
        }
        if z > 0 {
            visit(frame.id(x, y, z - 1));
        }
        if z + 1 < nz {
            visit(frame.id(x, y, z + 1));
        }
    }
    outside
}
/// Mean surface position of the occupied cells sharing a lattice corner, in grid coordinates.
/// A boundary face always has an occupied cell behind it, since the flood fill would otherwise
/// have crossed it, so every corner used by the surface has at least one.
fn corner_position(grid: &Grid, corner: [usize; 3]) -> [f64; 3] {
    let frame = &grid.frame;
    let (mut sum, mut count) = ([0.; 3], 0.);
    let range = |k: usize| corner[k].saturating_sub(1)..=corner[k].min(frame.dims[k] - 1);
    for z in range(2) {
        for y in range(1) {
            for x in range(0) {
                let cell = &grid.cells[frame.id(x, y, z)];
                if cell.occupied() {
                    let mean = cell.mean();
                    for (k, c) in [x, y, z].into_iter().enumerate() {
                        sum[k] += c as f64 + mean[k];
                    }
                    count += 1.;
                }
            }
        }
    }
    sum.map(|s| s / count)
}
pub fn extract(grid: &Grid) -> Res<Mesh> {
    let frame = &grid.frame;
    let [nx, ny, nz] = frame.dims;
    let outside = exterior(grid);
    let lattice = |c: [usize; 3]| c[0] + (nx + 1) * (c[1] + (ny + 1) * c[2]);
    let mut vertex_at = vec![u32::MAX; (nx + 1) * (ny + 1) * (nz + 1)];
    let mut vertices: Vec<[f64; 3]> = Vec::new();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    let area = |a: [f64; 3], b: [f64; 3], c: [f64; 3]| {
        let u = std::array::from_fn(|k| b[k] - a[k]);
        let v = std::array::from_fn(|k| c[k] - a[k]);
        let n = cross(u, v);
        0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
    };
    for id in 0..frame.len() {
        if outside[id] {
            continue;
        }
        let cell = frame.coords(id);
        for axis in 0..3 {
            for high in [false, true] {
                let mut neighbour = cell;
                match high {
                    true if cell[axis] + 1 < frame.dims[axis] => neighbour[axis] += 1,
                    false if cell[axis] > 0 => neighbour[axis] -= 1,
                    _ => continue,
                }
                if !outside[frame.id(neighbour[0], neighbour[1], neighbour[2])] {
                    continue;
                }
                let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                let mut corners = [cell; 4];
                for c in &mut corners {
                    c[axis] += usize::from(high);
                }
                corners[1][u] += 1;
                corners[2][u] += 1;
                corners[2][v] += 1;
                corners[3][v] += 1;
                // u x v points along +axis, so the low side reverses to keep facing outward.
                if !high {
                    corners.swap(1, 3);
                }
                let quad = corners.map(|c| {
                    let slot = &mut vertex_at[lattice(c)];
                    if *slot == u32::MAX {
                        *slot = vertices.len() as u32;
                        vertices.push(corner_position(grid, c));
                    }
                    *slot
                });
                let p = quad.map(|i| vertices[i as usize]);
                let squared =
                    |a: [f64; 3], b: [f64; 3]| (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f64>();
                // The shorter diagonal gives better-shaped triangles once vertices have moved.
                let split = if squared(p[0], p[2]) <= squared(p[1], p[3]) {
                    [[0, 1, 2], [0, 2, 3]]
                } else {
                    [[0, 1, 3], [1, 2, 3]]
                };
                for [a, b, c] in split {
                    if area(p[a], p[b], p[c]) > MIN_AREA {
                        triangles.push([quad[a], quad[b], quad[c]]);
                    }
                }
            }
        }
    }
    if triangles.is_empty() {
        return Err("No surface remains at this resolution; try a higher --resolution".into());
    }
    let face_normal = |t: &[u32; 3]| {
        let [a, b, c] = t.map(|i| vertices[i as usize]);
        cross(
            std::array::from_fn(|k| b[k] - a[k]),
            std::array::from_fn(|k| c[k] - a[k]),
        )
    };
    let mut normals = vec![[0f64; 3]; vertices.len()];
    for t in &triangles {
        let n = face_normal(t);
        for i in t {
            for k in 0..3 {
                normals[*i as usize][k] += n[k];
            }
        }
    }
    // A single projection would stretch the squares into streaks on surfaces running along it, so
    // each face projects along the axis it faces most. A vertex is written once per axis using it,
    // and shares its smooth normal across those seams. Slivers dropped above can orphan a vertex,
    // so only referenced ones are written.
    let period = (frame.dims.iter().max().unwrap() - 2) as f64 / REPEATS;
    let mut written = vec![[u32::MAX; 3]; vertices.len()];
    let mut mesh = Mesh {
        positions: Vec::new(),
        normals: Vec::new(),
        uvs: Vec::new(),
        indices: Vec::with_capacity(triangles.len() * 3),
    };
    for t in &triangles {
        let n = face_normal(t).map(f64::abs);
        let axis = (0..3).fold(0, |best, k| if n[k] > n[best] { k } else { best });
        for &i in t {
            let slot = &mut written[i as usize][axis];
            if *slot == u32::MAX {
                *slot = mesh.positions.len() as u32;
                let g = vertices[i as usize];
                mesh.positions.push(std::array::from_fn(|k| {
                    (frame.origin[k] + g[k] * frame.cell) as f32
                }));
                let n = normals[i as usize];
                let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                mesh.normals.push(if length > 0. {
                    n.map(|c| (c / length) as f32)
                } else {
                    [0., 1., 0.]
                });
                // Grid coordinates stay small and exact, however far from the origin the model is.
                mesh.uvs.push([
                    (g[(axis + 1) % 3] / period) as f32,
                    (g[(axis + 2) % 3] / period) as f32,
                ]);
            }
            mesh.indices.push(*slot);
        }
    }
    Ok(mesh)
}
