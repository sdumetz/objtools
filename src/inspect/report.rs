//! The statistics report.
use crate::shared::format::fmt_count;
fn ratio(used: u64, total: u64) -> String {
    if total == 0 {
        "-".into()
    } else {
        format!("{:.3}", used as f64 / total as f64)
    }
}
pub fn report<'a>(
    vertices: u64,
    faces: u64,
    textures: impl Iterator<Item = (&'a str, u32, u32, u64, u64)>,
) -> String {
    let textures: Vec<_> = textures.collect();
    let used: u64 = textures.iter().map(|t| t.3).sum();
    let total: u64 = textures.iter().map(|t| t.4).sum();
    let plural = if textures.len() == 1 { "" } else { "s" };
    let mut out = format!(
        "Vertices:           {}\nFaces:              {}\nDiffuse texels:     {} in {} texture{plural}\nTexture occupancy:  {}\n",
        fmt_count(vertices),
        fmt_count(faces),
        fmt_count(total),
        textures.len(),
        ratio(used, total),
    );
    if !textures.is_empty() {
        let width = textures
            .iter()
            .map(|t| t.0.chars().count())
            .max()
            .unwrap()
            .max(7);
        out.push_str(&format!(
            "\n{:width$}  {:>11}  Occupancy\n",
            "Texture", "Size"
        ));
        for (name, w, h, used, total) in textures {
            let size = format!("{w}x{h}");
            out.push_str(&format!(
                "{name:width$}  {size:>11}  {}\n",
                ratio(used, total)
            ));
        }
    }
    out
}
