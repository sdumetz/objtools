//! A glTF or GLB opened for reading. Binary data stays mapped from disk. The JSON is on the heap,
//! so `data:` URIs are decoded to the workspace on opening, and their text released.
mod accessor;
mod data_uri;
pub mod extensions;
pub mod matrix;

use super::{
    disk::{map, parent},
    failure::Context,
    Res,
};
pub use accessor::Accessor;
use memmap2::Mmap;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{BufRead, Seek},
    ops::Range,
    path::{Path, PathBuf},
};
const GLB_JSON: usize = 0x4e4f_534a;
const GLB_BIN: usize = 0x004e_4942;
pub struct Gltf {
    pub root: Value,
    base: PathBuf,
    buffers: Vec<(Mmap, Range<usize>)>,
    /// Images that were `data:` URIs, by image index.
    embedded: BTreeMap<usize, Mmap>,
}
/// Where an image's bytes are.
pub enum Image<'a> {
    File(PathBuf),
    Bytes(&'a [u8]),
}
pub fn index(value: &Value, what: &str) -> Res<usize> {
    let n = value
        .as_u64()
        .ok_or_else(|| format!("Missing or invalid {what} index"))?;
    Ok(usize::try_from(n)?)
}
pub fn element<'a>(root: &'a Value, array: &str, i: usize) -> Res<&'a Value> {
    root[array]
        .get(i)
        .ok_or_else(|| format!("{array}[{i}] does not exist").into())
}
/// KTX2 and AVIF sizes, read from the container: the `image` crate can't open either format
/// without a decoder. `None` for any other format, or for a container without a 2D size.
pub fn container_size(reader: impl BufRead + Seek) -> Option<(u32, u32)> {
    let size = imagesize::reader_size(reader).ok()?;
    let size = (size.width.try_into().ok()?, size.height.try_into().ok()?);
    (size.0 > 0 && size.1 > 0).then_some(size)
}
/// URI path component escaping (not filesystem escaping).
pub fn uri(name: &str) -> String {
    let mut out = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
/// Undo URI path escaping; `uri` writes `%XX` for every byte outside the unreserved set.
fn decode_uri(uri: &str) -> Res<PathBuf> {
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3).ok_or("Truncated URI escape")?)?;
            out.push(u8::from_str_radix(hex, 16)?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(PathBuf::from(String::from_utf8(out)?))
}
/// The byte ranges of a GLB's JSON and BIN chunks. Unknown chunks are skipped, as the spec asks.
fn glb_chunks(file: &[u8]) -> Res<(Range<usize>, Option<Range<usize>>)> {
    let field = |offset: usize| -> Res<usize> {
        let bytes = file.get(offset..offset + 4).ok_or("Truncated GLB")?;
        Ok(u32::from_le_bytes(bytes.try_into()?) as usize)
    };
    let version = field(4)?;
    if version != 2 {
        return Err(format!("Unsupported GLB version {version}").into());
    }
    let end = field(8)?;
    if end > file.len() {
        return Err("Truncated GLB".into());
    }
    let (mut json, mut bin, mut offset) = (None, None, 12);
    while offset < end {
        let (length, kind, start) = (field(offset)?, field(offset + 4)?, offset + 8);
        let stop = start
            .checked_add(length)
            .filter(|stop| *stop <= end)
            .ok_or("Truncated GLB")?;
        match kind {
            GLB_JSON if json.is_none() => json = Some(start..stop),
            GLB_BIN if bin.is_none() => bin = Some(start..stop),
            _ => {}
        }
        offset = stop;
    }
    Ok((json.ok_or("GLB has no JSON chunk")?, bin))
}
impl Gltf {
    /// A GLB is recognized by its content rather than its file extension, as in three.js's
    /// `GLTFLoader`. Extensions are checked first, so a file needing an unsupported one reports
    /// that rather than a symptom such as a buffer without data.
    pub fn open(path: &Path, workspace: &Path) -> Res<Self> {
        let in_file = || path.display().to_string();
        let file = map(path).with_context(in_file)?;
        let (mut root, mut bin): (Value, _) = if file.starts_with(b"glTF") {
            let (json, bin) = glb_chunks(&file).with_context(in_file)?;
            let root = serde_json::from_slice(&file[json]).with_context(in_file)?;
            (root, bin.map(|range| (file, range)))
        } else {
            let root = serde_json::from_slice(&file).with_context(in_file)?;
            (root, None)
        };
        extensions::check(&root)?;
        let base = parent(path).to_owned();
        let mut buffers = Vec::new();
        let list = root["buffers"].as_array_mut().into_iter().flatten();
        for (i, buffer) in list.enumerate() {
            let mut label = format!("Buffer {i}");
            let (map, range) = match buffer["uri"].as_str() {
                Some(uri) if uri.starts_with("data:") => {
                    let file = workspace.join(format!("buffer-{i}"));
                    let map = data_uri::decode(uri, &file).with_context(|| label.clone())?;
                    let whole = 0..map.len();
                    buffer.as_object_mut().unwrap().remove("uri");
                    (map, whole)
                }
                Some(uri) => {
                    let file = base.join(decode_uri(uri)?);
                    label = file.display().to_string();
                    let map = map(&file).with_context(|| label.clone())?;
                    let whole = 0..map.len();
                    (map, whole)
                }
                // Only the first buffer can mean the GLB's BIN chunk.
                None if i == 0 && bin.is_some() => bin.take().unwrap(),
                None => return Err(format!("{label} has no URI and no GLB chunk").into()),
            };
            if (range.len() as u64) < buffer["byteLength"].as_u64().unwrap_or(0) {
                return Err(format!("{label} is shorter than its byteLength").into());
            }
            buffers.push((map, range));
        }
        let mut embedded = BTreeMap::new();
        let list = root["images"].as_array_mut().into_iter().flatten();
        for (i, image) in list.enumerate() {
            if let Some(uri) = image["uri"].as_str().filter(|u| u.starts_with("data:")) {
                let file = workspace.join(format!("image-{i}"));
                let map = data_uri::decode(uri, &file).with_context(|| format!("Image {i}"))?;
                embedded.insert(i, map);
                image.as_object_mut().unwrap().remove("uri");
            }
        }
        Ok(Self {
            root,
            base,
            buffers,
            embedded,
        })
    }
    /// A buffer view's bytes, bounds-checked against its buffer, and its stride.
    pub fn view(&self, id: usize) -> Res<(&[u8], Option<usize>)> {
        let v = element(&self.root, "bufferViews", id)?;
        let (map, range) = self
            .buffers
            .get(index(&v["buffer"], "buffer")?)
            .ok_or("Buffer view references a missing buffer")?;
        let start = usize::try_from(v["byteOffset"].as_u64().unwrap_or(0))?;
        let length = index(&v["byteLength"], "byteLength")?;
        let bytes = start
            .checked_add(length)
            .and_then(|end| map[range.clone()].get(start..end))
            .ok_or("Buffer view exceeds its buffer")?;
        let stride = v["byteStride"].as_u64().map(usize::try_from).transpose()?;
        Ok((bytes, stride))
    }
    pub fn image(&self, id: usize) -> Res<Image<'_>> {
        if let Some(map) = self.embedded.get(&id) {
            return Ok(Image::Bytes(map));
        }
        let image = element(&self.root, "images", id)?;
        if let Some(uri) = image["uri"].as_str() {
            return Ok(Image::File(self.base.join(decode_uri(uri)?)));
        }
        let (bytes, _) = self.view(index(&image["bufferView"], "bufferView")?)?;
        Ok(Image::Bytes(bytes))
    }
}
