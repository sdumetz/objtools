//! Node and instance transforms. Column-major, like glTF's own `matrix`.
use crate::shared::{
    gltf::{index, Accessor, Gltf},
    Res,
};
use serde_json::Value;
pub type Matrix = [f64; 16];
pub const IDENTITY: Matrix = [
    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
];
pub fn multiply(a: &Matrix, b: &Matrix) -> Matrix {
    let mut out = [0.; 16];
    for column in 0..4 {
        for row in 0..4 {
            out[column * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[column * 4 + k]).sum();
        }
    }
    out
}
pub fn apply(m: &Matrix, p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| m[i] * p[0] + m[4 + i] * p[1] + m[8 + i] * p[2] + m[12 + i])
}
pub fn trs(t: [f64; 3], [x, y, z, w]: [f64; 4], s: [f64; 3]) -> Matrix {
    [
        (1. - 2. * (y * y + z * z)) * s[0],
        2. * (x * y + z * w) * s[0],
        2. * (x * z - y * w) * s[0],
        0.,
        2. * (x * y - z * w) * s[1],
        (1. - 2. * (x * x + z * z)) * s[1],
        2. * (y * z + x * w) * s[1],
        0.,
        2. * (x * z + y * w) * s[2],
        2. * (y * z - x * w) * s[2],
        (1. - 2. * (x * x + y * y)) * s[2],
        0.,
        t[0],
        t[1],
        t[2],
        1.,
    ]
}
pub fn numbers<const N: usize>(node: &Value, key: &str, default: [f64; N]) -> Res<[f64; N]> {
    let Some(value) = node.get(key) else {
        return Ok(default);
    };
    let invalid = || format!("Node {key} must be {N} numbers");
    let list = value
        .as_array()
        .filter(|a| a.len() == N)
        .ok_or_else(invalid)?;
    let mut out = default;
    for (slot, item) in out.iter_mut().zip(list) {
        *slot = item.as_f64().ok_or_else(invalid)?;
    }
    Ok(out)
}
pub fn local_matrix(node: &Value) -> Res<Matrix> {
    if node.get("matrix").is_some() {
        return numbers(node, "matrix", IDENTITY);
    }
    Ok(trs(
        numbers(node, "translation", [0.; 3])?,
        numbers(node, "rotation", [0., 0., 0., 1.])?,
        numbers(node, "scale", [1.; 3])?,
    ))
}
/// A node's instancing attributes: translation, rotation and scale, each optional.
pub type Instancing<'a> = [Option<Accessor<'a>>; 3];
pub fn instancing<'a>(gltf: &'a Gltf, node: &Value) -> Res<Option<Instancing<'a>>> {
    let attributes = &node["extensions"]["EXT_mesh_gpu_instancing"]["attributes"];
    if attributes.is_null() {
        return Ok(None);
    }
    let read = |name: &str| -> Res<Option<Accessor<'a>>> {
        let id = attributes.get(name);
        id.map(|id| Accessor::new(gltf, index(id, name)?))
            .transpose()
    };
    Ok(Some([
        read("TRANSLATION")?,
        read("ROTATION")?,
        read("SCALE")?,
    ]))
}
/// How many instances a node's attributes describe, which `verify` has checked they agree on.
pub fn instance_count(attributes: &Instancing) -> usize {
    attributes
        .iter()
        .flatten()
        .map(|a| a.count)
        .next()
        .unwrap_or(0)
}
/// Instance `i`'s matrix, relative to its node.
pub fn instance_matrix([t, r, s]: &Instancing, i: usize) -> Matrix {
    let read =
        |a: &Option<Accessor>, default: f64, c: usize| a.as_ref().map_or(default, |a| a.get(i, c));
    trs(
        std::array::from_fn(|c| read(t, 0., c)),
        [
            read(r, 0., 0),
            read(r, 0., 1),
            read(r, 0., 2),
            read(r, 1., 3),
        ],
        std::array::from_fn(|c| read(s, 1., c)),
    )
}
