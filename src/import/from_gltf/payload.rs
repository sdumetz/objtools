//! The output's binary data, in the workspace, and the JSON describing it. Views are copied whole
//! and renumbered; nothing in this file decides what the output contains.
use crate::shared::{
    gltf::{element, index, Gltf},
    Res,
};
use memmap2::MmapMut;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
};
pub struct Payload<'a> {
    gltf: &'a Gltf,
    pub out: BufWriter<File>,
    pub offset: u64,
    pub views: Vec<Value>,
    pub accessors: Vec<Value>,
    view_ids: HashMap<usize, usize>,
    accessor_ids: HashMap<usize, usize>,
}
/// The payload once written, for patching and for the document.
pub struct Output {
    pub views: Vec<Value>,
    pub accessors: Vec<Value>,
    pub accessor_ids: HashMap<usize, usize>,
    pub length: u64,
}
impl Output {
    /// Where a float VEC3 accessor's elements start in the payload, and their stride.
    pub fn location(&self, id: usize) -> (usize, usize) {
        let accessor = &self.accessors[id];
        let view = &self.views[accessor["bufferView"].as_u64().unwrap() as usize];
        let offset = accessor["byteOffset"].as_u64().unwrap_or(0);
        let start = view["byteOffset"].as_u64().unwrap() + offset;
        let stride = view["byteStride"].as_u64().unwrap_or(12);
        (start as usize, stride as usize)
    }
}
impl<'a> Payload<'a> {
    pub fn create(gltf: &'a Gltf, path: &Path) -> Res<Self> {
        Ok(Self {
            gltf,
            out: BufWriter::with_capacity(256 * 1024, File::create(path)?),
            offset: 0,
            views: Vec::new(),
            accessors: Vec::new(),
            view_ids: HashMap::new(),
            accessor_ids: HashMap::new(),
        })
    }
    /// Reach the next offset with this remainder modulo 4.
    pub fn pad(&mut self, residue: u64) -> Res<()> {
        let padding = (residue + 4 - self.offset % 4) % 4;
        self.out.write_all(&[0; 3][..padding as usize])?;
        self.offset += padding;
        Ok(())
    }
    /// A view's offset keeps its alignment, which accessors' own offsets may rely on.
    fn view(&mut self, old: usize) -> Res<usize> {
        if let Some(id) = self.view_ids.get(&old) {
            return Ok(*id);
        }
        let mut view = element(&self.gltf.root, "bufferViews", old)?.clone();
        let (bytes, _) = self.gltf.view(old)?;
        self.pad(view["byteOffset"].as_u64().unwrap_or(0) % 4)?;
        view["buffer"] = json!(0);
        view["byteOffset"] = json!(self.offset);
        self.out.write_all(bytes)?;
        self.offset += bytes.len() as u64;
        self.views.push(view);
        let id = self.views.len() - 1;
        self.view_ids.insert(old, id);
        Ok(id)
    }
    /// Copies an accessor once, however many things use it, so sharing survives.
    pub fn accessor(&mut self, old: usize) -> Res<usize> {
        if let Some(id) = self.accessor_ids.get(&old) {
            return Ok(*id);
        }
        let mut accessor = element(&self.gltf.root, "accessors", old)?.clone();
        if let Some(view) = accessor.get("bufferView") {
            accessor["bufferView"] = json!(self.view(index(view, "bufferView")?)?);
        }
        if accessor.get("sparse").is_some() {
            for part in ["indices", "values"] {
                let view = index(&accessor["sparse"][part]["bufferView"], "sparse bufferView")?;
                accessor["sparse"][part]["bufferView"] = json!(self.view(view)?);
            }
        }
        self.accessors.push(accessor);
        let id = self.accessors.len() - 1;
        self.accessor_ids.insert(old, id);
        Ok(id)
    }
    /// Renumbers a name-to-accessor map, copying what it names.
    pub fn attributes(&mut self, attributes: &mut Value) -> Res<()> {
        for (_, id) in attributes.as_object_mut().into_iter().flatten() {
            *id = json!(self.accessor(index(id, "attribute accessor")?)?);
        }
        Ok(())
    }
    /// The accessors no primitive names.
    pub fn other_accessors(
        &mut self,
        nodes: &mut [Value],
        skins: &mut [Value],
        animations: &mut [Value],
    ) -> Res<()> {
        for node in nodes {
            let attributes = node
                .get_mut("extensions")
                .and_then(|e| e.get_mut("EXT_mesh_gpu_instancing"))
                .and_then(|e| e.get_mut("attributes"));
            if let Some(attributes) = attributes {
                self.attributes(attributes)?;
            }
        }
        for skin in skins {
            if let Some(id) = skin.get("inverseBindMatrices") {
                let id = self.accessor(index(id, "inverseBindMatrices")?)?;
                skin["inverseBindMatrices"] = json!(id);
            }
        }
        for animation in animations {
            for sampler in animation["samplers"].as_array_mut().into_iter().flatten() {
                for key in ["input", "output"] {
                    let id = self.accessor(index(&sampler[key], "animation sampler")?)?;
                    sampler[key] = json!(id);
                }
            }
        }
        Ok(())
    }
    /// Ends writing. The caller has flushed `out`, which closes here.
    pub fn close(self) -> Output {
        Output {
            views: self.views,
            accessors: self.accessors,
            accessor_ids: self.accessor_ids,
            length: self.offset,
        }
    }
}
/// The finished workspace payload, writable in place.
pub struct Patches(MmapMut);
impl Patches {
    pub fn open(path: &Path) -> Res<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        // SAFETY: the payload is in the private workspace and only accessed through this mapping
        // until it is published.
        Ok(Self(unsafe { MmapMut::map_mut(&file)? }))
    }
    pub fn read_vec3(&self, at: usize) -> [f32; 3] {
        std::array::from_fn(|c| {
            let o = at + c * 4;
            f32::from_le_bytes(self.0[o..o + 4].try_into().unwrap())
        })
    }
    pub fn write_vec3(&mut self, at: usize, v: [f64; 3]) {
        for (c, x) in v.iter().enumerate() {
            let o = at + c * 4;
            self.0[o..o + 4].copy_from_slice(&(*x as f32).to_le_bytes());
        }
    }
    pub fn flush(&self) -> Res<()> {
        Ok(self.0.flush()?)
    }
}
