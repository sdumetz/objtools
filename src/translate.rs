use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};

use crate::fixed::{
    format_fixed_as_decimal, format_triple, parse_decimal_fixed, parse_three_fixed, BBox, SCALE,
};
use crate::format::fmt_bytes;

pub struct TranslateOptions {
    pub file_path: String,
    pub output: Option<String>,
    pub progress: bool,
    // origin provided as three numeric strings (will be parsed inside `run`)
    pub origin: Option<[String; 3]>,
    // compute the origin from the model's bounding box centre (adds a first pass)
    pub center: bool,
}

/// Payload of a `v` line, or `None` if this is not one (`vt`/`vn` must not match).
fn vertex_payload(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('v')?;
    if rest.starts_with(char::is_whitespace) {
        Some(rest.trim_start())
    } else {
        None
    }
}

/// First pass: stream the file and accumulate the bounding box of every vertex.
/// Constant memory, like everything else here — only six integers are retained.
fn scan_bbox(file_path: &str, progress: bool) -> Result<BBox, Box<dyn std::error::Error>> {
    let input = File::open(file_path).map_err(|e| format!("{}: {}", file_path, e))?;
    let reader = BufReader::with_capacity(64 * 1024, input);

    let mut bbox = BBox::new();
    let mut bytes_read: u64 = 0;
    let mut next_progress: u64 = 100_000_000;

    for line_res in reader.lines() {
        let line = line_res?;
        bytes_read += line.len() as u64 + 1;
        if progress && bytes_read >= next_progress {
            next_progress = bytes_read + 100_000_000;
            eprintln!("Pass 1 (bounding box): read {}", fmt_bytes(bytes_read));
        }
        if let Some(rest) = vertex_payload(&line) {
            // A malformed vertex is passed through untouched by pass 2, so it must not
            // abort the scan either.
            if let Ok(coords) = parse_three_fixed(rest) {
                bbox.add(coords);
            }
        }
    }

    Ok(bbox)
}


pub fn run(opts: TranslateOptions) -> Result<(), Box<dyn std::error::Error>> {
    let input = File::open(&opts.file_path).map_err(|e| format!("{}: {}", opts.file_path, e))?;
    let reader = BufReader::with_capacity(64 * 1024, input);

    let out: Box<dyn Write> = match &opts.output {
        Some(p) => Box::new(BufWriter::new(File::create(p)?)),
        None => Box::new(BufWriter::new(std::io::stdout())),
    };
    let mut out = out;

    // Reference point in fixed units (1e-10 m). Left as None to mean "use the first vertex".
    let mut first_found: Option<[i64; 3]> = None;

    if let Some(origin_strs) = &opts.origin {
        let coords = [
            parse_decimal_fixed(&origin_strs[0])?,
            parse_decimal_fixed(&origin_strs[1])?,
            parse_decimal_fixed(&origin_strs[2])?,
        ];
        first_found = Some(coords);
        eprintln!(
            "Using provided origin: {} m — translation magnitude: {:.10} m",
            format_triple(coords),
            magnitude(coords)
        );
    } else if opts.center {
        let bbox = scan_bbox(&opts.file_path, opts.progress)?;
        let coords = bbox
            .center()
            .ok_or_else(|| format!("{}: no vertices found, nothing to center", opts.file_path))?;
        eprintln!(
            "Bounding box: {} m ({} m)",
            bbox.range_str().unwrap_or_default(),
            bbox.size_str().unwrap_or_default()
        );
        first_found = Some(coords);
        eprintln!(
            "Using bounding-box center as origin: {} m — translation magnitude: {:.10} m",
            format_triple(coords),
            magnitude(coords)
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
        if let Some(rest) = vertex_payload(&line) {
            match parse_three_fixed(rest) {
                Ok(coords) => {
                    if first_found.is_none() {
                        first_found = Some(coords);
                        eprintln!("Reference point: {} m", format_triple(coords));
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


/// Distance from the origin, in metres. Only ever used for the human-readable log line, so an
/// f64 is fine here.
fn magnitude(coords: [i64; 3]) -> f64 {
    let c = |v: i64| v as f64 / SCALE as f64;
    (c(coords[0]).powi(2) + c(coords[1]).powi(2) + c(coords[2]).powi(2)).sqrt()
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
            center: false,
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

    #[test]
    fn translate_center_uses_bounding_box_midpoint() {
        let input_path = make_temp_path("center_in.obj");
        let output_path = make_temp_path("center_out.obj");

        let mut f = File::create(&input_path).expect("create input");
        // Deliberately not sorted, and the first vertex is not the centre, so a pass that
        // anchored on the first vertex would give a different answer.
        writeln!(f, "v 1000010 2000000 3000000").unwrap();
        writeln!(f, "vn 0 0 1").unwrap();
        writeln!(f, "v 1000000 2000004 3000000").unwrap();
        writeln!(f, "v 1000020 2000000 3000000").unwrap();
        f.flush().unwrap();

        let opts = TranslateOptions {
            file_path: input_path.to_string_lossy().into_owned(),
            output: Some(output_path.to_string_lossy().into_owned()),
            progress: false,
            origin: None,
            center: true,
        };
        run(opts).expect("translate run");

        // bbox is 1000000..1000020, 2000000..2000004, 3000000..3000000
        // => centre 1000010 2000002 3000000
        let out = read_to_string(&output_path).expect("read output");
        let v: Vec<&str> = out.lines().filter(|l| l.starts_with("v ")).collect();
        assert_eq!(v, vec!["v 0 -2 0", "v -10 2 0", "v 10 -2 0"]);
        // non-vertex lines pass through, and `vn` is not mistaken for `v`
        assert!(out.lines().any(|l| l == "vn 0 0 1"));

        let _ = std::fs::remove_file(&input_path);
        let _ = std::fs::remove_file(&output_path);
    }

    #[test]
    fn translate_center_rejects_a_file_with_no_vertices() {
        let input_path = make_temp_path("empty_in.obj");
        let mut f = File::create(&input_path).expect("create input");
        writeln!(f, "# nothing but a comment").unwrap();
        f.flush().unwrap();

        let opts = TranslateOptions {
            file_path: input_path.to_string_lossy().into_owned(),
            output: Some(make_temp_path("empty_out.obj").to_string_lossy().into_owned()),
            progress: false,
            origin: None,
            center: true,
        };
        let err = run(opts).expect_err("should refuse to centre an empty file");
        assert!(err.to_string().contains("no vertices"), "{}", err);

        let _ = std::fs::remove_file(&input_path);
    }
}
