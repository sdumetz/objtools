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
/// Counts with SI multipliers and three significant digits: 950, 12.4k, 268M, 1.07G.
pub fn fmt_count(n: u64) -> String {
    const UNITS: [&str; 5] = ["", "k", "M", "G", "T"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 999.5 && unit + 1 < UNITS.len() {
        value /= 1000.;
        unit += 1;
    }
    if unit == 0 {
        return n.to_string();
    }
    let decimals = if value < 9.995 {
        2
    } else if value < 99.95 {
        1
    } else {
        0
    };
    format!("{value:.decimals$}{}", UNITS[unit])
}
#[cfg(test)]
mod tests {
    use super::fmt_count;
    #[test]
    fn counts_use_three_significant_digits() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1000), "1.00k");
        assert_eq!(fmt_count(9_996), "10.0k");
        assert_eq!(fmt_count(12_449), "12.4k");
        assert_eq!(fmt_count(999_600), "1.00M");
        assert_eq!(fmt_count(268_435_456), "268M");
        assert_eq!(fmt_count(4_372_662), "4.37M");
        assert_eq!(fmt_count(1_073_741_824), "1.07G");
        assert_eq!(fmt_count(2_500_000_000_000_000), "2500T");
    }
}
