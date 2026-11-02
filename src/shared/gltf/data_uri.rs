//! `data:` URIs, decoded to a workspace file rather than a second copy on the heap.
use crate::shared::{disk::map, Res};
use memmap2::Mmap;
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};
pub fn decode(uri: &str, path: &Path) -> Res<Mmap> {
    let (header, payload) = uri
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .ok_or("Malformed data: URI")?;
    if !header.to_ascii_lowercase().ends_with(";base64") {
        return Err("Only base64 data: URIs are supported".into());
    }
    let mut out = BufWriter::new(File::create(path)?);
    if base64(payload.as_bytes(), &mut out)? == 0 {
        return Err("Empty data: URI".into());
    }
    out.flush()?;
    drop(out);
    map(path)
}
/// Returns the number of bytes written. Follows the forgiving decoding of the WHATWG Infra
/// standard, which browsers apply to `data:` URIs: whitespace is skipped and padding is optional.
fn base64(input: &[u8], out: &mut impl Write) -> Res<u64> {
    let (mut bits, mut held, mut chars, mut written) = (0u32, 0u32, 0u64, 0u64);
    let mut padding = false;
    for &c in input {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                padding = true;
                continue;
            }
            b' ' | b'\t' | b'\n' | b'\x0c' | b'\r' => continue,
            _ => return Err("Invalid base64 in data: URI".into()),
        };
        if padding {
            return Err("Invalid base64 in data: URI".into());
        }
        bits = bits << 6 | u32::from(value);
        held += 6;
        chars += 1;
        if held >= 8 {
            held -= 8;
            out.write_all(&[(bits >> held) as u8])?;
            written += 1;
        }
    }
    if chars % 4 == 1 {
        return Err("Truncated base64 in data: URI".into());
    }
    Ok(written)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn decoded(input: &str) -> Res<Vec<u8>> {
        let mut out = Vec::new();
        base64(input.as_bytes(), &mut out)?;
        Ok(out)
    }
    #[test]
    fn base64_follows_forgiving_decoding() {
        assert_eq!(decoded("").unwrap(), b"");
        assert_eq!(decoded("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(decoded("Zm9vYmE=").unwrap(), b"fooba");
        assert_eq!(decoded("Zm9vYg==").unwrap(), b"foob");
        assert_eq!(decoded("Zm9vYg").unwrap(), b"foob");
        assert_eq!(decoded(" Zm9v\r\nYmFy\t").unwrap(), b"foobar");
        assert_eq!(decoded("+/+/").unwrap(), [0xfb, 0xff, 0xbf]);
        assert!(decoded("Zm9v!").is_err());
        assert!(decoded("Zg==Zg==").is_err());
        assert!(decoded("Zm9vY").is_err());
        assert!(decoded("Zm9v-_").is_err());
    }
}
