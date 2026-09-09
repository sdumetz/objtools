pub fn fmt_verts(n: u64) -> String {
    if n >= 1_000_000 {
        let m = n as f64 / 1_000_000.0;
        if m == m.floor() {
            format!("{}M", m as u64)
        } else {
            format!("{:.1}M", m)
        }
    } else if n >= 1_000 {
        let k = n as f64 / 1_000.0;
        if k == k.floor() {
            format!("{}k", k as u64)
        } else {
            format!("{:.1}k", k)
        }
    } else {
        format!("{}", n)
    }
}

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
