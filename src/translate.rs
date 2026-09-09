use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};

pub struct TranslateOptions {
    pub file_path: String,
    pub output: Option<String>,
    pub progress: bool,
    // origin provided as three numeric strings (will be parsed inside `run`)
    pub origin: Option<[String; 3]>,
}


pub fn run(opts: TranslateOptions) -> Result<(), Box<dyn std::error::Error>> {
    let input = File::open(&opts.file_path).map_err(|e| format!("{}: {}", opts.file_path, e))?;
    let reader = BufReader::with_capacity(64 * 1024, input);

    let out: Box<dyn Write> = match &opts.output {
        Some(p) => Box::new(BufWriter::new(File::create(p)?)),
        None => Box::new(BufWriter::new(std::io::stdout())),
    };
    let mut out = out;

    let mut first_found: Option<[i64; 3]> = None; // reference point in fixed units (1e-10 m)

    // If origin was provided via options (strings), parse them now into fixed-point
    if let Some(origin_strs) = &opts.origin {
        let a = parse_decimal_fixed(&origin_strs[0])?;
        let b = parse_decimal_fixed(&origin_strs[1])?;
        let c = parse_decimal_fixed(&origin_strs[2])?;
        first_found = Some([a, b, c]);
        let coords = first_found.unwrap();
        let rx = format_fixed_as_decimal(coords[0]);
        let ry = format_fixed_as_decimal(coords[1]);
        let rz = format_fixed_as_decimal(coords[2]);
        let fx = coords[0] as f64 / 10_000_000_000.0;
        let fy = coords[1] as f64 / 10_000_000_000.0;
        let fz = coords[2] as f64 / 10_000_000_000.0;
        let mag = (fx * fx + fy * fy + fz * fz).sqrt();
        eprintln!(
            "Using provided origin: {} {} {} m — translation magnitude: {:.10} m",
            rx, ry, rz, mag
        );
    }

    let mut bytes_read: u64 = 0;
    let mut next_progress: u64 = 100_000_000;

    for line_res in reader.lines() {
        let line = line_res?;
        bytes_read += line.len() as u64 + 1;
        if opts.progress && bytes_read >= next_progress {
            next_progress = bytes_read + 100_000_000;
            eprintln!("Read {} bytes (approx)", bytes_read);
        }

        // Preserve original line endings/whitespace except for vertex translation
        let trimmed_start = line.trim_start();
        if trimmed_start.starts_with('v') && trimmed_start.split_whitespace().next() == Some("v") {
            // parse rest
            let rest = trimmed_start.get(1..).unwrap_or("").trim_start();
            match parse_three_fixed(rest) {
                Ok(coords) => {
                    if first_found.is_none() {
                        first_found = Some(coords);
                        // Log the reference point
                        let rx = format_fixed_as_decimal(coords[0]);
                        let ry = format_fixed_as_decimal(coords[1]);
                        let rz = format_fixed_as_decimal(coords[2]);
                        eprintln!(
                            "Reference point: {} {} {} m",
                            rx, ry, rz
                        );
                    }
                    let refpt = first_found.unwrap();
                    let dx = coords[0] - refpt[0];
                    let dy = coords[1] - refpt[1];
                    let dz = coords[2] - refpt[2];
                    let sx = format_fixed_as_decimal(dx);
                    let sy = format_fixed_as_decimal(dy);
                    let sz = format_fixed_as_decimal(dz);
                    writeln!(out, "v {} {} {}", sx, sy, sz)?;
                }
                Err(_) => {
                    // if parsing fails, pass-through original line
                    writeln!(out, "{}", line)?;
                }
            }
        } else {
            writeln!(out, "{}", line)?;
        }
    }

    // flush
    out.flush()?;
    Ok(())
}


// Parse three whitespace-separated decimal numbers into fixed-point (i64) with 10 decimal places
fn parse_three_fixed(s: &str) -> Result<[i64; 3], Box<dyn std::error::Error>> {
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

// Parse a decimal string into fixed-point i64 with 10 decimal places, attempting exact parsing
fn parse_decimal_fixed(s: &str) -> Result<i64, Box<dyn std::error::Error>> {
    let s = s.trim();
    if s.is_empty() { return Err("empty".into()); }

    // quick path: if contains 'e' or 'E', fall back to f64
    if s.contains('e') || s.contains('E') {
        let f: f64 = s.parse()?;
        let val = (f * 10_000_000_000.0).round();
        return Ok(val as i64);
    }

    // manual parse: sign, integer part, fractional part
    let (neg, body) = if s.starts_with('-') { (true, &s[1..]) } else if s.starts_with('+') { (false, &s[1..]) } else { (false, s) };
    let mut parts = body.splitn(2, '.');
    let int_part = parts.next().unwrap_or("");
    let frac_part = parts.next().unwrap_or("");

    let int_val: i128 = if int_part.is_empty() { 0 } else { int_part.parse::<i128>()? };

    // take up to 11 fractional digits to round to 10
    let mut frac_digits = frac_part.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
    if frac_digits.len() < 11 {
        while frac_digits.len() < 11 { frac_digits.push('0'); }
    }
    if frac_digits.len() > 11 {
        frac_digits.truncate(11);
    }

    // extract first 10 and the 11th for rounding
    let first10 = &frac_digits[0..10];
    let round_digit = frac_digits.chars().nth(10).unwrap_or('0');
    let mut frac_val: i128 = first10.parse::<i128>().unwrap_or(0);
    if round_digit >= '5' {
        frac_val += 1;
        if frac_val >= 10_000_000_000i128 {
            frac_val = 0;
            let carried = int_val + 1;
            let mut total = carried * 10_000_000_000i128 + frac_val;
            if neg { total = -total; }
            return Ok(total as i64);
        }
    }

    let mut total = int_val * 10_000_000_000i128 + frac_val;
    if neg { total = -total; }
    Ok(total as i64)
}
fn format_fixed_as_decimal(val: i64) -> String {
    // Format fixed-point value (scale = 1e10) and trim trailing zeros.
    if val == 0 { return "0".to_string(); }
    let neg = val < 0;
    let a = if neg { (-val) as i128 } else { val as i128 };
    let intp = (a / 10_000_000_000i128) as i128;
    let frac = (a % 10_000_000_000i128) as i128;
    if frac == 0 {
        if neg { format!("-{}", intp) } else { format!("{}", intp) }
    } else {
        let mut frac_s = format!("{:010}", frac);
        // trim trailing zeros
        while frac_s.ends_with('0') { frac_s.pop(); }
        if neg {
            format!("-{}.{}", intp, frac_s)
        } else {
            format!("{}.{}", intp, frac_s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, read_to_string};
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_temp_path(suffix: &str) -> std::path::PathBuf {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let mut p = std::env::temp_dir();
        p.push(format!("objtools_test_{}_{}", now, suffix));
        p
    }

    #[test]
    fn translate_basic_vertices() {
        let input_path = make_temp_path("in.obj");
        let output_path = make_temp_path("out.obj");

        let mut f = File::create(&input_path).expect("create input");
        // large world coordinates; fixed uses 10 decimal places
        writeln!(f, "# test").unwrap();
        // realistic trimmed inputs (no unnecessary trailing zeros)
        writeln!(f, "v 1000000 2000000 3000000").unwrap();
        writeln!(f, "v 1000001 2000002.5 3000000").unwrap();
        f.flush().unwrap();

        let opts = TranslateOptions {
            file_path: input_path.to_string_lossy().into_owned(),
            output: Some(output_path.to_string_lossy().into_owned()),
            progress: false,
            origin: None,
        };

        run(opts).expect("translate run");

        let out = read_to_string(&output_path).expect("read output");
        let lines: Vec<&str> = out.lines().collect();
        // skip the comment line
        assert!(lines.len() >= 3);
        // find the two v lines
        let v1 = lines.iter().find(|l| l.starts_with("v ")).unwrap();
        assert_eq!(*v1, "v 0 0 0");

        // find second v line (may be last)
        let v_lines: Vec<&&str> = lines.iter().filter(|l| l.starts_with("v ")).collect();
        assert_eq!(v_lines.len(), 2);
        assert_eq!(v_lines[1].trim(), "v 1 2.5 0");

        // cleanup
        let _ = std::fs::remove_file(&input_path);
        let _ = std::fs::remove_file(&output_path);
    }
}
