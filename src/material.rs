//! Conservative Phong -> PBR policy: no inferred metals, explicit specular extension.
use crate::import::Res;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};
#[derive(Clone, Debug)]
pub struct Material {
    pub name: String,
    pub kd: [f64; 3],
    pub ks: [f64; 3],
    pub ke: [f64; 3],
    pub ns: f64,
    pub opacity: f64,
    pub illum: u32,
    pub ior: f64,
    pub metallic: f64,
    pub roughness: Option<f64>,
    pub texture: Option<PathBuf>,
    pub double_sided: bool,
    dissolve: bool,
}
impl Material {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            kd: [0.8; 3],
            ks: [0.04; 3],
            ke: [0.; 3],
            ns: 0.,
            opacity: 1.,
            illum: 2,
            ior: 1.5,
            metallic: 0.,
            roughness: None,
            texture: None,
            double_sided: false,
            dissolve: false,
        }
    }
    pub fn json(&self, texture: Option<usize>) -> Value {
        let mut pbr = json!({"baseColorFactor": [self.kd[0], self.kd[1], self.kd[2], self.opacity], "metallicFactor": self.metallic,
            "roughnessFactor": self.roughness.unwrap_or_else(|| (2. / (self.ns + 2.)).powf(0.25))});
        if let Some(i) = texture {
            pbr["baseColorTexture"] = json!({"index": i});
        }
        let mut extensions = serde_json::Map::new();
        if self.illum == 0 {
            extensions.insert("KHR_materials_unlit".into(), json!({}));
        } else {
            let f0 = ((self.ior - 1.) / (self.ior + 1.)).powi(2);
            let color = if f0 > 0. {
                self.ks.map(|k| k / f0)
            } else {
                [0.; 3]
            };
            extensions.insert("KHR_materials_specular".into(), json!({"specularFactor": if self.illum == 1 {0.} else {1.}, "specularColorFactor": color}));
            if self.ior != 1.5 {
                extensions.insert("KHR_materials_ior".into(), json!({"ior": self.ior}));
            }
        }
        json!({"name": self.name, "pbrMetallicRoughness": pbr, "emissiveFactor": self.ke, "doubleSided": self.double_sided,
            "alphaMode": if self.opacity < 1. {"BLEND"} else {"OPAQUE"}, "extensions": extensions,
            "extras": {"sourcePhong": {"Kd": self.kd, "Ks": self.ks, "Ke": self.ke, "Ns": self.ns, "Ni": self.ior, "illum": self.illum}}})
    }
}
fn finite(s: &str) -> Res<f64> {
    let n: f64 = s.parse()?;
    if !n.is_finite() {
        return Err("Non-finite material value".into());
    }
    Ok(n)
}
fn color(rest: &str) -> Res<[f64; 3]> {
    let v: Vec<_> = rest.split_whitespace().collect();
    if v.len() != 3 {
        return Err("Expected three RGB material components (spectral/xyz unsupported)".into());
    }
    Ok([
        finite(v[0])?.clamp(0., 1.),
        finite(v[1])?.clamp(0., 1.),
        finite(v[2])?.clamp(0., 1.),
    ])
}
pub fn load(
    path: &Path,
    texture_base: Option<&Path>,
    materials: &mut HashMap<String, Material>,
) -> Res<()> {
    let reader = BufReader::new(File::open(path).map_err(|e| format!("{}: {e}", path.display()))?);
    let mut current: Option<Material> = None;
    let mut unsupported = std::collections::HashSet::new();
    for (line_no, line) in reader.lines().enumerate() {
        let line = line?;
        let line = line.split('#').next().unwrap().trim();
        let (key, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        if key == "newmtl" {
            if let Some(m) = current.take() {
                materials.insert(m.name.clone(), m);
            }
            current = Some(Material::new(rest.trim()));
            continue;
        }
        let Some(m) = current.as_mut() else { continue };
        let rest = rest.trim();
        let result: Res<()> = (|| {
            match key {
                "Kd" => m.kd = color(rest)?,
                "Ks" => m.ks = color(rest)?,
                "Ke" => m.ke = color(rest)?,
                "Ns" => m.ns = finite(rest)?.max(0.),
                "Ni" => {
                    m.ior = finite(rest)?;
                    if m.ior < 1. {
                        return Err("Ni must be >= 1".into());
                    }
                }
                "d" => {
                    if rest.starts_with('-') {
                        return Err("d -halo is unsupported".into());
                    }
                    m.opacity = finite(rest)?.clamp(0., 1.);
                    m.dissolve = true;
                }
                "Tr" if !m.dissolve => m.opacity = 1. - finite(rest)?.clamp(0., 1.),
                "Tr" => {}
                "illum" => m.illum = rest.parse()?,
                "Pm" => m.metallic = finite(rest)?.clamp(0., 1.),
                "objtools_doubleSided" => m.double_sided = rest == "1",
                "Pr" => m.roughness = Some(finite(rest)?.clamp(0., 1.)),
                "map_Kd" => {
                    if rest.starts_with('-') {
                        return Err(
                            "map_Kd options are unsupported; bake the texture transform first"
                                .into(),
                        );
                    }
                    let name = rest.trim_matches('"').replace('\\', "/");
                    m.texture = Some(
                        texture_base
                            .unwrap_or_else(|| path.parent().unwrap_or(Path::new(".")))
                            .join(name),
                    );
                }
                "" | "Ka" => {}
                _ => {
                    if unsupported.insert(key.to_owned()) {
                        eprintln!(
                            "Warning: unsupported MTL property {key} in {}",
                            path.display()
                        );
                    }
                }
            }
            Ok(())
        })();
        result.map_err(|e| format!("{}:{}: {e}", path.display(), line_no + 1))?;
    }
    if let Some(m) = current {
        materials.insert(m.name.clone(), m);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn phong_policy() {
        let mut m = Material::new("paint");
        m.ks = [0.2, 0.1, 0.04];
        m.ns = 126.;
        let j = m.json(None);
        assert_eq!(j["pbrMetallicRoughness"]["metallicFactor"], 0.);
        assert!(
            (j["pbrMetallicRoughness"]["roughnessFactor"]
                .as_f64()
                .unwrap()
                - 0.35355339059)
                .abs()
                < 1e-9
        );
        assert!(
            (j["extensions"]["KHR_materials_specular"]["specularColorFactor"][0]
                .as_f64()
                .unwrap()
                - 5.)
                .abs()
                < 1e-12
        );
        m.illum = 1;
        assert_eq!(
            m.json(None)["extensions"]["KHR_materials_specular"]["specularFactor"],
            0.
        );
        m.illum = 0;
        assert!(m.json(None)["extensions"]["KHR_materials_unlit"].is_object());
    }
}
