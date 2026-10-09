//! Source image policy and optional decoding/encoding, one image at a time.
use crate::{import::Res, material::Material};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufWriter, Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};
#[derive(Clone, Copy, Default)]
pub enum Mode {
    #[default]
    Auto,
    Keep,
    Copy,
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
    pub alpha: bool,
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
                Mode::Keep => false,
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
        let alpha = texture_alpha(&content, output_ext)
            .map_err(|e| format!("Texture {}: {e}", source.display()))?;
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
                alpha,
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
// Inspect PNG transparency without decoding or retaining the image in memory.
pub fn texture_alpha(path: &Path, extension: &str) -> Res<bool> {
    let mut file = File::open(path)?;
    if extension != "png" {
        let mut magic = [0; 2];
        file.read_exact(&mut magic)?;
        if magic != [0xff, 0xd8] {
            return Err("JPEG texture has an invalid signature".into());
        }
        return Ok(false);
    }
    let mut signature = [0; 8];
    file.read_exact(&mut signature)?;
    if &signature != b"\x89PNG\r\n\x1a\n" {
        return Err("PNG texture has an invalid signature".into());
    }
    let length = file.metadata()?.len();
    loop {
        let mut header = [0; 8];
        file.read_exact(&mut header)?;
        let size = u32::from_be_bytes(header[..4].try_into().unwrap()) as u64;
        if file
            .stream_position()?
            .checked_add(size + 4)
            .is_none_or(|end| end > length)
        {
            return Err("Truncated PNG texture".into());
        }
        match &header[4..] {
            b"IHDR" => {
                if size != 13 {
                    return Err("Invalid PNG IHDR".into());
                }
                let mut ihdr = [0; 13];
                file.read_exact(&mut ihdr)?;
                if matches!(ihdr[9], 4 | 6) {
                    return Ok(true);
                }
                file.seek(SeekFrom::Current(4))?;
            }
            b"tRNS" => return Ok(true),
            b"IDAT" | b"IEND" => return Ok(false),
            _ => {
                file.seek(SeekFrom::Current((size + 4) as i64))?;
            }
        }
    }
}
