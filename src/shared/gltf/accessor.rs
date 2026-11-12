//! Typed reads of a glTF accessor, without copying its data out of the mapped buffers.
use super::{element, index, Gltf};
use crate::shared::{failure::Context, Res};
use serde_json::Value;
use std::cmp::Ordering;
pub struct Accessor<'a> {
    /// `None` without a buffer view: the spec's base values are then all zeros.
    data: Option<&'a [u8]>,
    pub count: usize,
    pub components: usize,
    stride: usize,
    width: usize,
    pub kind: u64,
    pub normalized: bool,
    sparse: Option<Sparse<'a>>,
}
impl<'a> Accessor<'a> {
    pub fn new(gltf: &'a Gltf, id: usize) -> Res<Self> {
        let a = element(&gltf.root, "accessors", id)?;
        let count = index(&a["count"], "count")?;
        let components = match a["type"].as_str() {
            Some("SCALAR") => 1,
            Some("VEC2") => 2,
            Some("VEC3") => 3,
            Some("VEC4") | Some("MAT2") => 4,
            Some("MAT3") => 9,
            Some("MAT4") => 16,
            _ => return Err(format!("Accessor {id}: unsupported type").into()),
        };
        let kind = a["componentType"].as_u64().unwrap_or(0);
        let width = match kind {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            5125 | 5126 => 4,
            _ => return Err(format!("Accessor {id}: unsupported component type").into()),
        };
        // Matrix columns start on 4-byte boundaries, which only pads narrow components.
        let size = match (a["type"].as_str(), width) {
            (Some("MAT2"), 1) => 8,
            (Some("MAT3"), 1) => 12,
            (Some("MAT3"), 2) => 24,
            _ => components * width,
        };
        let (data, stride) = match a.get("bufferView") {
            None => (None, size),
            Some(view) => {
                let (bytes, stride) = gltf.view(index(view, "bufferView")?)?;
                let stride = stride.unwrap_or(size);
                let start = usize::try_from(a["byteOffset"].as_u64().unwrap_or(0))?;
                let needed = match count {
                    0 => 0,
                    n => (n - 1)
                        .checked_mul(stride)
                        .and_then(|n| n.checked_add(size))
                        .ok_or("Accessor size overflow")?,
                };
                let data = start
                    .checked_add(needed)
                    .and_then(|end| bytes.get(start..end))
                    .ok_or_else(|| format!("Accessor {id} exceeds its buffer view"))?;
                (Some(data), stride)
            }
        };
        let sparse = match a.get("sparse") {
            None => None,
            Some(sparse) => Some(
                Sparse::new(gltf, sparse, count, size).with_context(|| format!("Accessor {id}"))?,
            ),
        };
        Ok(Self {
            data,
            count,
            components,
            stride,
            width,
            kind,
            normalized: a["normalized"].as_bool().unwrap_or(false),
            sparse,
        })
    }
    /// Whether every element is stored at its own place in a buffer view.
    pub fn is_dense(&self) -> bool {
        self.data.is_some() && self.sparse.is_none()
    }
    /// The element's value as stored, before normalization.
    pub fn raw(&self, i: usize, c: usize) -> f64 {
        let size = self.components * self.width;
        let replaced = self.sparse.as_ref().and_then(|sparse| {
            sparse
                .find(i)
                .map(|position| (sparse.values, position * size))
        });
        let (data, start) = match (replaced, self.data) {
            (Some(found), _) => found,
            (None, Some(data)) => (data, i * self.stride),
            (None, None) => return 0.,
        };
        let o = start + c * self.width;
        let b = &data[o..o + self.width];
        match self.kind {
            5120 => f64::from(b[0] as i8),
            5121 => f64::from(b[0]),
            5122 => f64::from(i16::from_le_bytes([b[0], b[1]])),
            5123 => f64::from(u16::from_le_bytes([b[0], b[1]])),
            5125 => f64::from(u32::from_le_bytes(b.try_into().unwrap())),
            _ => f64::from(f32::from_le_bytes(b.try_into().unwrap())),
        }
    }
    pub fn get(&self, i: usize, c: usize) -> f64 {
        let raw = self.raw(i, c);
        if !self.normalized {
            return raw;
        }
        let max = match self.kind {
            5120 => 127.,
            5121 => 255.,
            5122 => 32767.,
            5123 => 65535.,
            _ => return raw,
        };
        (raw / max).max(-1.)
    }
}
/// Elements replaced over an accessor's base values. They are looked up in the mapped index list
/// on each read, so a sparse accessor needs no heap or disk beyond its file.
struct Sparse<'a> {
    indices: &'a [u8],
    width: usize,
    values: &'a [u8],
    count: usize,
}
/// `length` bytes of a sparse section, which the spec requires to be tightly packed.
fn section<'a>(gltf: &'a Gltf, part: &Value, length: usize) -> Res<&'a [u8]> {
    let (bytes, _) = gltf.view(index(&part["bufferView"], "sparse bufferView")?)?;
    let start = usize::try_from(part["byteOffset"].as_u64().unwrap_or(0))?;
    start
        .checked_add(length)
        .and_then(|end| bytes.get(start..end))
        .ok_or_else(|| "Sparse data exceeds its buffer view".into())
}
impl<'a> Sparse<'a> {
    fn new(gltf: &'a Gltf, sparse: &Value, total: usize, size: usize) -> Res<Self> {
        let count = index(&sparse["count"], "sparse count")?;
        if count == 0 || count > total {
            return Err(format!("Sparse count {count} is invalid for {total} elements").into());
        }
        let width = match sparse["indices"]["componentType"].as_u64() {
            Some(5121) => 1,
            Some(5123) => 2,
            Some(5125) => 4,
            _ => return Err("Sparse indices must be unsigned integers".into()),
        };
        let bytes = |n: usize| n.checked_mul(count).ok_or("Sparse size overflow");
        let this = Self {
            indices: section(gltf, &sparse["indices"], bytes(width)?)?,
            width,
            values: section(gltf, &sparse["values"], bytes(size)?)?,
            count,
        };
        // A binary search relies on this, and the spec requires it.
        let mut previous = None;
        for position in 0..count {
            let i = this.index(position);
            if previous.is_some_and(|p| i <= p) || i >= total {
                return Err("Sparse indices must increase and stay within the accessor".into());
            }
            previous = Some(i);
        }
        Ok(this)
    }
    fn index(&self, position: usize) -> usize {
        let b = &self.indices[position * self.width..][..self.width];
        match *b {
            [a] => a.into(),
            [a, c] => u16::from_le_bytes([a, c]).into(),
            _ => u32::from_le_bytes(b.try_into().unwrap()) as usize,
        }
    }
    /// The position of element `i` in the values, if it is replaced.
    fn find(&self, i: usize) -> Option<usize> {
        let (mut low, mut high) = (0, self.count);
        while low < high {
            let middle = low + (high - low) / 2;
            match self.index(middle).cmp(&i) {
                Ordering::Equal => return Some(middle),
                Ordering::Less => low = middle + 1,
                Ordering::Greater => high = middle,
            }
        }
        None
    }
}
