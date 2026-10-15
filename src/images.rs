//! Source image policy and optional decoding/encoding, one image at a time.
use crate::{import::Res, material::Material};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufReader, BufWriter, Read},
    path::{Component, Path, PathBuf},
    thread::{self, JoinHandle},
};
#[derive(Clone, Copy, Default)]
pub enum Mode {
    #[default]
    Auto,
    Keep,
    Copy,
    /// Internal to `inspect`: reference every source image as-is, whatever its format, without
    /// conversion or validation. The glTF is never published, only read back.
    Reference,
}
impl Mode {
    pub fn parse(s: &str) -> Res<Self> {
        match s {
            "auto" => Ok(Self::Auto),
            "keep" => Ok(Self::Keep),
            "copy" => Ok(Self::Copy),
            _ => Err("--images must be auto, keep or copy".into()),
        }
    }
}
#[derive(Clone, Copy, Default)]
pub enum Format {
    #[default]
    Jpg,
    Png,
}
impl Format {
    pub fn parse(s: &str) -> Res<Self> {
        match s {
            "jpg" | "jpeg" => Ok(Self::Jpg),
            "png" => Ok(Self::Png),
            _ => Err("--image-format must be jpg or png".into()),
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Jpg => "jpg",
            Self::Png => "png",
        }
    }
}
pub struct Options {
    pub mode: Mode,
    pub convert: Option<bool>,
    pub format: Format,
    pub quality: u8,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            mode: Mode::Auto,
            convert: None,
            format: Format::Jpg,
            quality: 95,
        }
    }
}
pub fn parse_convert(s: &str) -> Res<Option<bool>> {
    match s {
        "auto" => Ok(None),
        "true" => Ok(Some(true)),
        "false" => Ok(Some(false)),
        _ => Err("--convert-images must be auto, true or false".into()),
    }
}
pub struct Plan {
    pub content: PathBuf,
    pub publish_name: Option<String>,
    pub uri: String,
    /// Whether the content is a published glTF image whose transparency must be scanned.
    scan: bool,
}
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
fn relative_uri(source: &Path, destination: &Path) -> Res<String> {
    let from: Vec<_> = destination.components().collect();
    let to: Vec<_> = source.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return Err(
            "Cannot reference source images across filesystem roots; use --images copy".into(),
        );
    }
    let mut parts = vec!["..".to_owned(); from.len() - common];
    for component in &to[common..] {
        if let Component::Normal(name) = component {
            parts.push(crate::gltf::uri(&name.to_string_lossy()));
        }
    }
    Ok(parts.join("/"))
}
pub fn prepare(
    options: &Options,
    input: &Path,
    output: &Path,
    workspace: &Path,
    materials: &[Material],
) -> Res<HashMap<PathBuf, Plan>> {
    let destination = fs::canonicalize(parent(output))?;
    let same_folder = fs::canonicalize(parent(input))? == destination;
    let stem = output
        .file_stem()
        .ok_or("Invalid output filename")?
        .to_string_lossy();
    let mut plans = HashMap::new();
    for material in materials {
        let Some(path) = &material.texture else {
            continue;
        };
        let source =
            fs::canonicalize(path).map_err(|e| format!("Texture {}: {e}", path.display()))?;
        if plans.contains_key(&source) {
            continue;
        }
        if let Mode::Reference = options.mode {
            let plan = Plan {
                content: source.clone(),
                publish_name: None,
                uri: relative_uri(&source, &destination)?,
                scan: false,
            };
            plans.insert(source, plan);
            continue;
        }
        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let supported = matches!(ext.as_str(), "jpg" | "jpeg" | "png");
        let convert = options.convert.unwrap_or(!supported);
        if !supported && !convert {
            return Err(format!("Texture {} has unsupported glTF image format .{ext} with --convert-images=false; enable conversion", source.display()).into());
        }
        let copy = convert
            || match options.mode {
                Mode::Auto => !same_folder,
                Mode::Keep | Mode::Reference => false,
                Mode::Copy => true,
            };
        let output_ext = if convert {
            options.format.extension()
        } else {
            &ext
        };
        let name = format!("{stem}_texture_{}.{output_ext}", plans.len());
        let content = if convert {
            let converted = workspace.join(&name);
            convert_image(&source, &converted, options)
                .map_err(|e| format!("Converting texture {}: {e}", source.display()))?;
            converted
        } else {
            source.clone()
        };
        check_image(&content).map_err(|e| format!("Texture {}: {e}", source.display()))?;
        let uri = if copy {
            crate::gltf::uri(&name)
        } else {
            relative_uri(&source, &destination)?
        };
        plans.insert(
            source,
            Plan {
                content,
                publish_name: copy.then_some(name),
                uri,
                scan: true,
            },
        );
    }
    Ok(plans)
}
fn convert_image(source: &Path, destination: &Path, options: &Options) -> Res<()> {
    let decoded = image::ImageReader::open(source)?
        .with_guessed_format()?
        .decode()?;
    let mut output = BufWriter::new(File::create(destination)?);
    match options.format {
        Format::Png => decoded.write_to(&mut output, image::ImageFormat::Png)?,
        Format::Jpg => {
            let rgb = if decoded.has_alpha() {
                let mut rgba = decoded.to_rgba8();
                if rgba.pixels().any(|p| p[3] != 255) {
                    eprintln!("Warning: converting {} to JPG composites transparency onto white; use --image-format png to preserve alpha", source.display());
                }
                for p in rgba.pixels_mut() {
                    for i in 0..3 {
                        p[i] = ((p[i] as u32 * p[3] as u32 + 255 * (255 - p[3] as u32) + 127) / 255)
                            as u8;
                    }
                }
                image::DynamicImage::ImageRgba8(rgba).to_rgb8()
            } else {
                decoded.to_rgb8()
            };
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, options.quality)
                .encode_image(&rgb)?;
        }
    }
    std::io::Write::flush(&mut output)?;
    Ok(())
}
/// The source paths of textures with any pixel below full opacity, scanned on a background
/// thread: inflating a large PNG takes seconds, and only the material JSON needs the answer.
pub fn scan_transparency(
    plans: &HashMap<PathBuf, Plan>,
) -> JoinHandle<Result<HashSet<PathBuf>, String>> {
    let jobs: Vec<_> = plans
        .iter()
        .filter(|(_, plan)| plan.scan)
        .map(|(source, plan)| (source.clone(), plan.content.clone()))
        .collect();
    thread::spawn(move || {
        let mut transparent = HashSet::new();
        for (source, content) in jobs {
            if texture_alpha(&content).map_err(|e| format!("Texture {}: {e}", source.display()))? {
                transparent.insert(source);
            }
        }
        Ok(transparent)
    })
}
/// Check the signature and, for PNG, the header chunks without reading any pixel, so a broken
/// texture fails the import before the geometry work.
fn check_image(path: &Path) -> Res<Option<png::Reader<BufReader<File>>>> {
    let mut file = BufReader::new(File::open(path)?);
    let png = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("png"));
    if !png {
        let mut magic = [0; 2];
        file.read_exact(&mut magic)?;
        if magic != [0xff, 0xd8] {
            return Err("JPEG texture has an invalid signature".into());
        }
        return Ok(None);
    }
    let mut decoder = png::Decoder::new(file);
    // Palette and tRNS transparency become an alpha channel; 16-bit samples stay exact.
    decoder.set_transformations(png::Transformations::EXPAND);
    Ok(Some(decoder.read_info()?))
}
/// Whether any pixel is less than fully opaque. An alpha channel alone proves nothing: baking and
/// photogrammetry tools routinely write RGBA PNGs whose alpha is 255 everywhere, and blending
/// those would cost sorting artefacts and a transparent pass for nothing. PNG rows are scanned
/// one at a time, so even a 16k texture costs only a row of memory.
fn texture_alpha(path: &Path) -> Res<bool> {
    let Some(mut reader) = check_image(path)? else {
        return Ok(false);
    };
    let (color, depth) = reader.output_color_type();
    if !matches!(color, png::ColorType::GrayscaleAlpha | png::ColorType::Rgba) {
        return Ok(false);
    }
    let sample = if depth == png::BitDepth::Sixteen {
        2
    } else {
        1
    };
    let pixel = color.samples() * sample;
    while let Some(row) = reader.next_row()? {
        if row
            .data()
            .chunks_exact(pixel)
            .any(|p| p[pixel - sample..].iter().any(|b| *b != 0xff))
        {
            return Ok(true);
        }
    }
    Ok(false)
}
#[cfg(test)]
mod tests {
    use super::*;
    use png::{BitDepth, ColorType};
    fn encode(
        dir: &Path,
        name: &str,
        color: ColorType,
        depth: BitDepth,
        setup: impl FnOnce(&mut png::Encoder<'_, BufWriter<File>>),
        data: &[u8],
    ) -> PathBuf {
        let path = dir.join(name);
        let mut encoder = png::Encoder::new(BufWriter::new(File::create(&path).unwrap()), 2, 1);
        encoder.set_color(color);
        encoder.set_depth(depth);
        setup(&mut encoder);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(data).unwrap();
        writer.finish().unwrap();
        path
    }
    #[test]
    fn alpha_means_a_pixel_below_full_opacity() {
        let dir = std::env::temp_dir().join(format!("objtools-alpha-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let (eight, sixteen) = (BitDepth::Eight, BitDepth::Sixteen);
        let none = |_: &mut png::Encoder<'_, BufWriter<File>>| {};
        let cases = [
            ("rgb", ColorType::Rgb, eight, vec![1, 2, 3, 4, 5, 6], false),
            (
                "rgba-opaque",
                ColorType::Rgba,
                eight,
                vec![1, 2, 3, 255, 4, 5, 6, 255],
                false,
            ),
            (
                "rgba-254",
                ColorType::Rgba,
                eight,
                vec![1, 2, 3, 255, 4, 5, 6, 254],
                true,
            ),
            (
                "ga-zero",
                ColorType::GrayscaleAlpha,
                eight,
                vec![255, 255, 0, 0],
                true,
            ),
            (
                "rgba16-opaque",
                ColorType::Rgba,
                sixteen,
                [255; 16].to_vec(),
                false,
            ),
        ];
        for (name, color, depth, data, expected) in cases {
            let path = encode(&dir, &format!("{name}.png"), color, depth, none, &data);
            assert_eq!(texture_alpha(&path).unwrap(), expected, "{name}");
        }
        // The low byte of a 16-bit alpha counts too.
        let mut data = [255; 16];
        data[15] = 254;
        let path = encode(
            &dir,
            "rgba16-low.png",
            ColorType::Rgba,
            sixteen,
            none,
            &data,
        );
        assert!(texture_alpha(&path).unwrap());
        // tRNS: only entries that pixels actually use matter.
        let palette = |e: &mut png::Encoder<'_, BufWriter<File>>| {
            e.set_palette(vec![0, 0, 0, 255, 255, 255]);
            e.set_trns(vec![255, 0]);
        };
        let path = encode(
            &dir,
            "indexed-opaque.png",
            ColorType::Indexed,
            eight,
            palette,
            &[0, 0],
        );
        assert!(!texture_alpha(&path).unwrap());
        let path = encode(
            &dir,
            "indexed-clear.png",
            ColorType::Indexed,
            eight,
            palette,
            &[0, 1],
        );
        assert!(texture_alpha(&path).unwrap());
        let key = |e: &mut png::Encoder<'_, BufWriter<File>>| e.set_trns(vec![0, 7]);
        let path = encode(
            &dir,
            "gray-key.png",
            ColorType::Grayscale,
            eight,
            key,
            &[3, 7],
        );
        assert!(texture_alpha(&path).unwrap());
        let path = encode(
            &dir,
            "gray-unkeyed.png",
            ColorType::Grayscale,
            eight,
            key,
            &[3, 8],
        );
        assert!(!texture_alpha(&path).unwrap());
        fs::write(dir.join("photo.jpg"), [0xff, 0xd8, 0xff]).unwrap();
        assert!(!texture_alpha(&dir.join("photo.jpg")).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }
}
