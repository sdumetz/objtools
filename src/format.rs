pub fn fmt_bytes(n: u64) -> String {
    if n >= 1_000_000_000 {
        let g = n as f64 / 1_000_000_000.0;
        format!("{:.1} GB", g)
    } else if n >= 1_000_000 {
        let m = n as f64 / 1_000_000.0;
        format!("{:.1} MB", m)
    } else {
        let k = n as f64 / 1_000.0;
        format!("{:.1} kB", k)
    }
}
