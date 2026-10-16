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
    /// `Tf`, the colour filter for light passing through a refracting material.
    transmission_filter: Option<[f64; 3]>,
    pub illum: u32,
    pub ior: f64,
    pub metallic: f64,
    pub roughness: Option<f64>,
    pub texture: Option<PathBuf>,
    /// `map_Kd -s` and `-o`, in MTL's UV space (V up): the texture is sampled at `s·uv + o`.
    uv_scale: [f64; 2],
    uv_offset: [f64; 2],
    /// `map_Kd -clamp on`: one copy of the texture rather than repeated copies.
    pub clamp: bool,
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
            transmission_filter: None,
            illum: 2,
            ior: 1.5,
            metallic: 0.,
            roughness: None,
            texture: None,
            uv_scale: [1.; 2],
            uv_offset: [0.; 2],
            clamp: false,
            double_sided: false,
            dissolve: false,
        }
    }
    /// `map_Kd -s`/`-o` as `KHR_texture_transform`, or `None` for the identity. glTF's V runs
    /// down (`v = 1 − v_mtl`), so sampling at `s_v·v_mtl + o_v` becomes `s_v·v + 1 − s_v − o_v`.
    fn uv_transform(&self) -> Option<Value> {
        let [su, sv] = self.uv_scale;
        let offset = [self.uv_offset[0], 1. - sv - self.uv_offset[1]];
        (self.uv_scale != [1.; 2] || offset != [0.; 2])
            .then(|| json!({"offset": offset, "scale": [su, sv]}))
    }
    /// MTL has two kinds of transparency, as glTF does. `d` dissolves the surface "unlike a real
    /// transparent material", which is glTF's alpha as coverage. Only the transparent `illum`
    /// models describe light passing through, which is `KHR_materials_transmission`:
    /// - 4 and 9, "Glass on": an almost transparent surface that keeps its highlights. There the
    ///   dissolve sets the transmission instead of the coverage.
    /// - 6 and 7, "Refraction on": light passes through filtered by `Tf`, whose mean gives the
    ///   transmission. glTF tints transmitted light with the base colour, so `Tf`'s own colour is
    ///   not kept. `d` still dissolves, since it "works on all illumination models".
    ///
    /// `Tf` under any other model is ignored. Some exporters write `Tf 1 1 1` on every material,
    /// and those materials must not turn into glass.
    /// Returns the coverage alpha and the transmission factor.
    fn transparency(&self) -> (f64, f64) {
        match self.illum {
            4 | 9 => (1., 1. - self.opacity),
            6 | 7 => {
                let tf = self.transmission_filter.unwrap_or([0.; 3]);
                (self.opacity, tf.iter().sum::<f64>() / 3.)
            }
            _ => (self.opacity, 0.),
        }
    }
    pub fn json(&self, texture: Option<usize>) -> Value {
        let (alpha, transmission) = self.transparency();
        let mut pbr = json!({"baseColorFactor": [self.kd[0], self.kd[1], self.kd[2], alpha], "metallicFactor": self.metallic,
            "roughnessFactor": self.roughness.unwrap_or_else(|| (2. / (self.ns + 2.)).powf(0.25))});
        if let Some(i) = texture {
            pbr["baseColorTexture"] = json!({"index": i});
            if let Some(transform) = self.uv_transform() {
                pbr["baseColorTexture"]["extensions"] = json!({"KHR_texture_transform": transform});
            }
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
        if transmission > 0. {
            extensions.insert(
                "KHR_materials_transmission".into(),
                json!({"transmissionFactor": transmission}),
            );
        }
        json!({"name": self.name, "pbrMetallicRoughness": pbr, "emissiveFactor": self.ke, "doubleSided": self.double_sided,
            "alphaMode": if alpha < 1. {"BLEND"} else {"OPAQUE"}, "extensions": extensions,
            "extras": {"sourcePhong": {"Kd": self.kd, "Ks": self.ks, "Ke": self.ke, "Ns": self.ns, "Ni": self.ior, "d": self.opacity, "Tf": self.transmission_filter, "illum": self.illum}}})
    }
}
fn take_token<'a>(rest: &mut &'a str) -> &'a str {
    let s = rest.trim_start();
    let (token, after) = s.split_once(char::is_whitespace).unwrap_or((s, ""));
    *rest = after;
    token
}
/// Between `min` and `max` numbers: the spec makes trailing components of some options optional.
fn take_numbers(rest: &mut &str, option: &str, min: usize, max: usize) -> Res<Vec<f64>> {
    let mut values = Vec::new();
    while values.len() < max {
        let mut probe = *rest;
        match take_token(&mut probe).parse::<f64>() {
            Ok(v) if v.is_finite() => {
                values.push(v);
                *rest = probe;
            }
            _ => break,
        }
    }
    if values.len() < min {
        return Err(format!("map_Kd {option} expects {min} number(s)").into());
    }
    Ok(values)
}
fn take_switch(rest: &mut &str, option: &str) -> Res<bool> {
    match take_token(rest) {
        "on" => Ok(true),
        "off" => Ok(false),
        other => Err(format!("map_Kd {option} expects on or off, not {other:?}").into()),
    }
}
/// Apply the options leading a `map_Kd` statement, the MTL spec's list for it plus a few that
/// exporters also write, and return the texture path that follows, with the options whose
/// non-default values cannot be represented and are dropped.
///
/// - `-s`/`-o` scale and offset the texture coordinates, becoming `KHR_texture_transform`. Both
///   three.js (`MTLLoader`: `texture.repeat`/`offset`) and Blender (a Mapping node) sample at
///   `s·uv + o`. The third, `w`, component has no meaning for a 2D image.
/// - `-clamp on` maps one copy of the texture instead of repeating it. glTF's closest sampler is
///   `CLAMP_TO_EDGE`: identical over `[0, 1]`, where it also stops bilinear filtering from
///   bleeding in the opposite edge, but it stretches the edge texels beyond, where MTL shows the
///   bare material. Neither three.js nor Blender applies `-clamp`.
/// - `-mm` (value remapping) and `-t` (turbulence) have no glTF equivalent and are reported.
/// - The rest do not affect a diffuse colour map, and are skipped like Blender skips them.
///
/// An option outside this list is an error: there is no telling how many arguments it takes, so
/// where the path starts.
fn texture_options<'a>(mut rest: &'a str, m: &mut Material) -> Res<(&'a str, Vec<&'static str>)> {
    let mut ignored = Vec::new();
    loop {
        let mut after = rest;
        let option = take_token(&mut after);
        if !option.starts_with('-') {
            return Ok((rest.trim(), ignored));
        }
        rest = after;
        match option {
            "-s" => {
                let v = take_numbers(&mut rest, option, 1, 3)?;
                m.uv_scale = [v[0], v.get(1).copied().unwrap_or(1.)];
            }
            "-o" => {
                let v = take_numbers(&mut rest, option, 1, 3)?;
                m.uv_offset = [v[0], v.get(1).copied().unwrap_or(0.)];
            }
            "-clamp" => m.clamp = take_switch(&mut rest, option)?,
            "-mm" => {
                if take_numbers(&mut rest, option, 2, 2)? != [0., 1.] {
                    ignored.push("-mm");
                }
            }
            "-t" => {
                if take_numbers(&mut rest, option, 1, 3)?
                    .iter()
                    .any(|t| *t != 0.)
                {
                    ignored.push("-t");
                }
            }
            "-blendu" | "-blendv" | "-cc" => {
                take_switch(&mut rest, option)?;
            }
            "-texres" | "-boost" | "-bm" => {
                take_numbers(&mut rest, option, 1, 1)?;
            }
            "-imfchan" => {
                take_token(&mut rest);
            }
            _ => return Err(format!("Unsupported map_Kd option {option}").into()),
        }
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
                // `Kt` is an alias some exporters write, read by Blender too.
                "Tf" | "Kt" => match color(rest) {
                    Ok(tf) => m.transmission_filter = Some(tf),
                    Err(e) => {
                        if unsupported.insert(key.to_owned()) {
                            eprintln!("Warning: {key} ignored in {}: {e}", path.display());
                        }
                    }
                },
                "Pm" => m.metallic = finite(rest)?.clamp(0., 1.),
                "objtools_doubleSided" => m.double_sided = rest == "1",
                "Pr" => m.roughness = Some(finite(rest)?.clamp(0., 1.)),
                "map_Kd" => {
                    let (rest, ignored) = texture_options(rest, m)?;
                    for option in ignored {
                        if unsupported.insert(format!("map_Kd {option}")) {
                            eprintln!("Warning: map_Kd {option} is ignored in {}", path.display());
                        }
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
    #[test]
    fn transparency_follows_illumination_models() {
        let mut m = Material::new("glass");
        m.opacity = 0.25;
        m.transmission_filter = Some([1.; 3]);
        let summary = |m: &Material| {
            let j = m.json(None);
            (
                j["pbrMetallicRoughness"]["baseColorFactor"][3]
                    .as_f64()
                    .unwrap(),
                j["alphaMode"].as_str().unwrap().to_owned(),
                j["extensions"]["KHR_materials_transmission"]["transmissionFactor"].as_f64(),
            )
        };
        // Dissolve is coverage, and `Tf` alone does not make glass.
        assert_eq!(summary(&m), (0.25, "BLEND".into(), None));
        // Glass: the dissolve becomes transmission, and the surface is all there.
        m.illum = 4;
        assert_eq!(summary(&m), (1., "OPAQUE".into(), Some(0.75)));
        m.illum = 9;
        m.opacity = 1.;
        assert_eq!(summary(&m), (1., "OPAQUE".into(), None));
        // Refraction: `Tf` transmits, and dissolve still applies.
        m.illum = 7;
        m.opacity = 0.5;
        m.transmission_filter = Some([0., 1., 0.5]);
        assert_eq!(summary(&m), (0.5, "BLEND".into(), Some(0.5)));
        m.transmission_filter = None;
        assert_eq!(summary(&m), (0.5, "BLEND".into(), None));
    }
}
