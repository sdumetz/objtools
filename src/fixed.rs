//! Fixed-point decimal coordinates.
//!
//! Georeferenced meshes carry coordinates in the millions of metres while still resolving
//! millimetres, which is right at the edge of what an `f64` round-trips cleanly. Coordinates are
//! therefore parsed straight from their decimal text into an `i64` scaled by 1e10, so comparisons
//! and differences are exact integer arithmetic and the digits that were in the file are the
//! digits that come back out.

use serde::Serialize;

/// One fixed-point unit, in metres: 1e-10 m (0.1 nm).
/// An `i64` at this scale spans roughly ±9.2e8 m, which covers any projected coordinate system.
pub const SCALE: i128 = 10_000_000_000;

/// Largest integer part that can still fit an `i64` once scaled.
const MAX_INT_PART: i128 = i64::MAX as i128 / SCALE + 1;

fn to_i64(total: i128, src: &str) -> Result<i64, Box<dyn std::error::Error>> {
    i64::try_from(total).map_err(|_| format!("coordinate out of range: {}", src).into())
}

/// Parse a decimal string into fixed-point. Exponent notation falls back to an `f64` parse.
pub fn parse_decimal_fixed(s: &str) -> Result<i64, Box<dyn std::error::Error>> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty".into());
    }

    // quick path: if contains 'e' or 'E', fall back to f64
    if s.contains('e') || s.contains('E') {
        let f: f64 = s.parse()?;
        let val = (f * SCALE as f64).round();
        return Ok(val as i64);
    }

    // Manual parse: sign, integer part, fractional part.
    // Deliberately allocation-free — this runs three times per vertex, so on a file with a
    // billion vertices a single String per coordinate is the difference between minutes.
    let (neg, body) = if let Some(rest) = s.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = s.strip_prefix('+') {
        (false, rest)
    } else {
        (false, s)
    };
    let bytes = body.as_bytes();

    let mut int_val: i128 = 0;
    let mut i = 0;
    while i < bytes.len() && bytes[i] != b'.' {
        let d = bytes[i];
        if !d.is_ascii_digit() {
            return Err(format!("not a number: {}", s).into());
        }
        int_val = int_val * 10 + (d - b'0') as i128;
        if int_val > MAX_INT_PART {
            return Err(format!("coordinate out of range: {}", s).into());
        }
        i += 1;
    }

    // Take up to 11 fractional digits: ten to keep, the eleventh to round on.
    let mut frac_val: i128 = 0;
    let mut ndigits = 0usize;
    let mut round_digit = b'0';
    if i < bytes.len() {
        for &d in &bytes[i + 1..] {
            if !d.is_ascii_digit() {
                continue;
            }
            if ndigits < 10 {
                frac_val = frac_val * 10 + (d - b'0') as i128;
                ndigits += 1;
            } else {
                round_digit = d;
                break;
            }
        }
    }
    while ndigits < 10 {
        frac_val *= 10;
        ndigits += 1;
    }

    if round_digit >= b'5' {
        frac_val += 1;
        if frac_val >= SCALE {
            let mut total = (int_val + 1) * SCALE;
            if neg {
                total = -total;
            }
            return to_i64(total, s);
        }
    }

    let mut total = int_val * SCALE + frac_val;
    if neg {
        total = -total;
    }
    to_i64(total, s)
}

/// Parse three whitespace-separated decimals into fixed-point.
pub fn parse_three_fixed(s: &str) -> Result<[i64; 3], Box<dyn std::error::Error>> {
    let mut it = s.split_whitespace();
    let x = it.next().ok_or("missing x")?;
    let y = it.next().ok_or("missing y")?;
    let z = it.next().ok_or("missing z")?;
    Ok([
        parse_decimal_fixed(x)?,
        parse_decimal_fixed(y)?,
        parse_decimal_fixed(z)?,
    ])
}

/// Format a fixed-point value back to decimal, trailing zeros trimmed.
pub fn format_fixed(val: i128) -> String {
    if val == 0 {
        return "0".to_string();
    }
    let neg = val < 0;
    let a = if neg { -val } else { val };
    let intp = a / SCALE;
    let frac = a % SCALE;
    let sign = if neg { "-" } else { "" };
    if frac == 0 {
        format!("{}{}", sign, intp)
    } else {
        let mut frac_s = format!("{:010}", frac);
        while frac_s.ends_with('0') {
            frac_s.pop();
        }
        format!("{}{}.{}", sign, intp, frac_s)
    }
}

pub fn format_fixed_as_decimal(val: i64) -> String {
    format_fixed(val as i128)
}

/// Format a triple of fixed-point values as `x y z`.
pub fn format_triple(v: [i64; 3]) -> String {
    format!(
        "{} {} {}",
        format_fixed_as_decimal(v[0]),
        format_fixed_as_decimal(v[1]),
        format_fixed_as_decimal(v[2])
    )
}

/// An axis-aligned bounding box in fixed-point coordinates.
///
/// Empty until the first vertex is added, so a group with no geometry reports no bounds rather
/// than a box collapsed on the origin.
#[derive(Clone, Copy, Default)]
pub struct BBox {
    bounds: Option<([i64; 3], [i64; 3])>,
}

impl BBox {
    pub fn new() -> Self {
        Self { bounds: None }
    }

    pub fn add(&mut self, c: [i64; 3]) {
        match &mut self.bounds {
            None => self.bounds = Some((c, c)),
            Some((min, max)) => {
                for i in 0..3 {
                    if c[i] < min[i] {
                        min[i] = c[i];
                    }
                    if c[i] > max[i] {
                        max[i] = c[i];
                    }
                }
            }
        }
    }

    pub fn merge(&mut self, other: &BBox) {
        if let Some((min, max)) = other.bounds {
            self.add(min);
            self.add(max);
        }
    }

    /// Extent along each axis. Widened to `i128` because `max - min` can exceed `i64` even
    /// when both ends are representable.
    pub fn size(&self) -> Option<[i128; 3]> {
        let (min, max) = self.bounds?;
        Some([
            max[0] as i128 - min[0] as i128,
            max[1] as i128 - min[1] as i128,
            max[2] as i128 - min[2] as i128,
        ])
    }

    /// Midpoint of the box — the reference point `translate --center` subtracts.
    /// Computed in `i128` so the intermediate sum cannot overflow; the result always lies inside
    /// the box and so is representable.
    pub fn center(&self) -> Option<[i64; 3]> {
        let (min, max) = self.bounds?;
        let mid = |a: i64, b: i64| ((a as i128 + b as i128) / 2) as i64;
        Some([mid(min[0], max[0]), mid(min[1], max[1]), mid(min[2], max[2])])
    }

    /// `min → max`, for human-readable output.
    pub fn range_str(&self) -> Option<String> {
        let (min, max) = self.bounds?;
        Some(format!("{} → {}", format_triple(min), format_triple(max)))
    }

    /// `x × y × z`, for human-readable output.
    pub fn size_str(&self) -> Option<String> {
        let s = self.size()?;
        Some(format!(
            "{} × {} × {}",
            format_fixed(s[0]),
            format_fixed(s[1]),
            format_fixed(s[2])
        ))
    }

    pub fn to_record(self) -> Option<BBoxRecord> {
        let (min, max) = self.bounds?;
        let size = self.size()?;
        let center = self.center()?;
        let f = |v: [i64; 3]| [
            format_fixed_as_decimal(v[0]),
            format_fixed_as_decimal(v[1]),
            format_fixed_as_decimal(v[2]),
        ];
        Some(BBoxRecord {
            min: f(min),
            max: f(max),
            size: [format_fixed(size[0]), format_fixed(size[1]), format_fixed(size[2])],
            center: f(center),
        })
    }
}

/// JSON view of a bounding box.
///
/// Coordinates are emitted as decimal *strings*: a georeferenced value carries more significant
/// digits than a JSON number survives in most parsers, and losing them here would defeat the point
/// of the fixed-point pipeline.
#[derive(Serialize)]
pub struct BBoxRecord {
    pub min: [String; 3],
    pub max: [String; 3],
    pub size: [String; 3],
    pub center: [String; 3],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats_round_trip() {
        for s in ["0", "1", "-1", "1000000.0000000001", "-0.5", "651234.5678901234"] {
            assert_eq!(format_fixed_as_decimal(parse_decimal_fixed(s).unwrap()), {
                // round-trip drops the redundant ".0" but keeps every significant digit
                let t = if s.contains('.') {
                    s.trim_end_matches('0').trim_end_matches('.')
                } else {
                    s
                };
                t.to_string()
            });
        }
    }

    #[test]
    fn rounds_the_eleventh_fractional_digit() {
        assert_eq!(parse_decimal_fixed("0.00000000005").unwrap(), 1);
        assert_eq!(parse_decimal_fixed("0.00000000004").unwrap(), 0);
        assert_eq!(parse_decimal_fixed("-0.99999999995").unwrap(), -SCALE as i64);
    }

    #[test]
    fn rejects_out_of_range_coordinates() {
        assert!(parse_decimal_fixed("1000000000").is_err());
        assert!(parse_decimal_fixed("99999999999999999999999999").is_err());
    }

    #[test]
    fn rejects_non_numeric_input() {
        assert!(parse_decimal_fixed("nan").is_err());
        assert!(parse_decimal_fixed("1a2").is_err());
        assert!(parse_decimal_fixed("").is_err());
    }

    #[test]
    fn handles_edge_shaped_decimals() {
        assert_eq!(format_fixed_as_decimal(parse_decimal_fixed(".5").unwrap()), "0.5");
        assert_eq!(format_fixed_as_decimal(parse_decimal_fixed("1.").unwrap()), "1");
        assert_eq!(format_fixed_as_decimal(parse_decimal_fixed("+2.25").unwrap()), "2.25");
        assert_eq!(format_fixed_as_decimal(parse_decimal_fixed("-0").unwrap()), "0");
        // digits past the tenth are dropped (the eleventh only rounds), and the formatter
        // trims the trailing zero
        assert_eq!(
            format_fixed_as_decimal(parse_decimal_fixed("0.123456789012345").unwrap()),
            "0.123456789"
        );
        assert_eq!(
            format_fixed_as_decimal(parse_decimal_fixed("0.123456789062345").unwrap()),
            "0.1234567891"
        );
    }

    #[test]
    fn bbox_is_empty_until_a_point_is_added() {
        let b = BBox::new();
        assert!(b.center().is_none());
        assert!(b.range_str().is_none());
        assert!(b.to_record().is_none());
    }

    #[test]
    fn bbox_tracks_extent_and_centre() {
        let mut b = BBox::new();
        b.add(parse_three_fixed("-1 0 2.5").unwrap());
        b.add(parse_three_fixed("3 0 -2.5").unwrap());
        b.add(parse_three_fixed("1 0 0").unwrap());

        assert_eq!(b.range_str().unwrap(), "-1 0 -2.5 → 3 0 2.5");
        assert_eq!(format_triple(b.center().unwrap()), "1 0 0");
        assert_eq!(b.size_str().unwrap(), "4 × 0 × 5");
    }

    #[test]
    fn bbox_centre_is_exact_for_large_coordinates() {
        // The whole reason for fixed point: an f64 midpoint of these two loses the last digits.
        let mut b = BBox::new();
        b.add(parse_three_fixed("651000.0000000001 0 0").unwrap());
        b.add(parse_three_fixed("651000.0000000003 0 0").unwrap());
        assert_eq!(format_triple(b.center().unwrap()), "651000.0000000002 0 0");
    }

    #[test]
    fn merge_combines_two_boxes() {
        let mut a = BBox::new();
        a.add(parse_three_fixed("0 0 0").unwrap());
        let mut b = BBox::new();
        b.add(parse_three_fixed("5 -5 1").unwrap());

        a.merge(&b);
        assert_eq!(a.range_str().unwrap(), "0 -5 0 → 5 0 1");

        // merging an empty box changes nothing
        a.merge(&BBox::new());
        assert_eq!(a.range_str().unwrap(), "0 -5 0 → 5 0 1");
    }
}
