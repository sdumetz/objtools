//! Ear clipping in the dominant plane; reject degenerate/untriangulatable polygons.
use crate::import::Res;
pub fn is_degenerate_triangle(points: &[[f64; 3]]) -> bool {
    if points.len() != 3 {
        return false;
    }
    let u: [f64; 3] = std::array::from_fn(|i| points[1][i] - points[0][i]);
    let v: [f64; 3] = std::array::from_fn(|i| points[2][i] - points[0][i]);
    [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ]
    .iter()
    .all(|n| *n == 0.)
}

pub fn triangulate(points: &[[f64; 3]]) -> Res<(Vec<[usize; 3]>, [f32; 3])> {
    let mut normal = [0.; 3];
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        normal[0] += (a[1] - b[1]) * (a[2] + b[2]);
        normal[1] += (a[2] - b[2]) * (a[0] + b[0]);
        normal[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    let length = normal.iter().map(|n| n * n).sum::<f64>().sqrt();
    if length == 0. || !length.is_finite() {
        return Err("Degenerate polygon".into());
    }
    let unit = normal.map(|n| (n / length) as f32);
    let drop = (0..3)
        .max_by(|a, b| normal[*a].abs().total_cmp(&normal[*b].abs()))
        .unwrap();
    let axes: Vec<_> = (0..3).filter(|a| *a != drop).collect();
    let p: Vec<[f64; 2]> = points.iter().map(|p| [p[axes[0]], p[axes[1]]]).collect();
    let cross = |a: usize, b: usize, c: usize| {
        (p[b][0] - p[a][0]) * (p[c][1] - p[a][1]) - (p[b][1] - p[a][1]) * (p[c][0] - p[a][0])
    };
    let area = (1..p.len() - 1).map(|i| cross(0, i, i + 1)).sum::<f64>();
    if area == 0. {
        return Err("Degenerate projected polygon".into());
    }
    let sign = area.signum();
    let mut remaining: Vec<usize> = (0..p.len()).collect();
    let mut triangles = Vec::with_capacity(p.len() - 2);
    while remaining.len() > 3 {
        let mut found = false;
        for i in 0..remaining.len() {
            let a = remaining[(i + remaining.len() - 1) % remaining.len()];
            let b = remaining[i];
            let c = remaining[(i + 1) % remaining.len()];
            if cross(a, b, c) * sign <= 0. {
                continue;
            }
            let contains = remaining.iter().any(|&j| {
                j != a
                    && j != b
                    && j != c
                    && cross(a, b, j) * sign >= 0.
                    && cross(b, c, j) * sign >= 0.
                    && cross(c, a, j) * sign >= 0.
            });
            if contains {
                continue;
            }
            triangles.push([a, b, c]);
            remaining.remove(i);
            found = true;
            break;
        }
        if !found {
            return Err(
                "Cannot triangulate polygon: degenerate, self-intersecting or unsupported".into(),
            );
        }
    }
    if cross(remaining[0], remaining[1], remaining[2]) * sign <= 0. {
        return Err("Degenerate final triangle".into());
    }
    triangles.push([remaining[0], remaining[1], remaining[2]]);
    Ok((triangles, unit))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concave_both_windings() {
        let mut p = vec![
            [0., 0., 0.],
            [2., 0., 0.],
            [2., 2., 0.],
            [1., 1., 0.],
            [0., 2., 0.],
        ];
        let (t, n) = triangulate(&p).unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(n, [0., 0., 1.]);
        p.reverse();
        assert_eq!(triangulate(&p).unwrap().1, [0., 0., -1.]);
    }
    #[test]
    fn rejects_degenerate() {
        assert!(triangulate(&[[0.; 3]; 3]).is_err());
    }
}
