//! The statistics report: one structure, serialized for `--json` and pretty-printed otherwise, so
//! that both outputs carry the same data.
use crate::shared::format::fmt_count;
use serde::Serialize;
use std::fmt;

#[derive(Debug, Serialize)]
pub struct Report {
    vertices: u64,
    faces: u64,
    diffuse_texels: u64,
    occupied_texels: u64,
    /// `None` when there are no texels to occupy.
    texture_occupancy: Option<f64>,
    textures: Vec<TextureReport>,
}
#[derive(Debug, Serialize)]
pub struct TextureReport {
    name: String,
    width: u32,
    height: u32,
    occupied_texels: u64,
    occupancy: Option<f64>,
}
fn occupancy(used: u64, total: u64) -> Option<f64> {
    (total != 0).then(|| used as f64 / total as f64)
}
impl TextureReport {
    pub fn new(name: String, width: u32, height: u32, occupied_texels: u64) -> Self {
        Self {
            name,
            width,
            height,
            occupied_texels,
            occupancy: occupancy(occupied_texels, Self::texels(width, height)),
        }
    }
    fn texels(width: u32, height: u32) -> u64 {
        u64::from(width) * u64::from(height)
    }
}
impl Report {
    pub fn new(vertices: u64, faces: u64, textures: Vec<TextureReport>) -> Self {
        let diffuse_texels = textures
            .iter()
            .map(|t| TextureReport::texels(t.width, t.height))
            .sum();
        let occupied_texels = textures.iter().map(|t| t.occupied_texels).sum();
        Self {
            vertices,
            faces,
            diffuse_texels,
            occupied_texels,
            texture_occupancy: occupancy(occupied_texels, diffuse_texels),
            textures,
        }
    }
}
fn fmt_occupancy(occupancy: Option<f64>) -> String {
    occupancy.map_or("-".into(), |o| format!("{o:.3}"))
}
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let count = self.textures.len();
        let plural = if count == 1 { "" } else { "s" };
        writeln!(f, "Vertices:           {}", fmt_count(self.vertices))?;
        writeln!(f, "Faces:              {}", fmt_count(self.faces))?;
        writeln!(
            f,
            "Diffuse texels:     {} in {count} texture{plural}",
            fmt_count(self.diffuse_texels),
        )?;
        writeln!(
            f,
            "Texture occupancy:  {}",
            fmt_occupancy(self.texture_occupancy)
        )?;
        let names = self.textures.iter().map(|t| t.name.chars().count());
        let Some(width) = names.max().map(|n| n.max(7)) else {
            return Ok(());
        };
        writeln!(f, "\n{:width$}  {:>11}  Occupancy", "Texture", "Size")?;
        for t in &self.textures {
            let size = format!("{}x{}", t.width, t.height);
            writeln!(
                f,
                "{:width$}  {size:>11}  {}",
                t.name,
                fmt_occupancy(t.occupancy)
            )?;
        }
        Ok(())
    }
}
