//! Everything objtools writes to stderr. A host may show it to whoever supplied the file, so it
//! has to be safe to show as it stands, with nothing left for the host to strip.
#[cfg(not(test))]
use std::io::{self, Write};

/// Long enough for any message objtools composes. Text from a file is capped well below this.
const MAX_LINE: usize = 512;

/// Writes one line to stderr, as `eprintln!` would.
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => {
        $crate::shared::diagnostics::line(&format!($($arg)*))
    };
}
/// `eprintln!` would panic once a host has closed the pipe, turning a warning into a failed import.
pub fn line(text: &str) {
    let mut line = sanitized(text);
    line.push('\n');
    #[cfg(test)]
    #[allow(clippy::print_stderr)]
    {
        // Only this macro's output is captured for a passing test.
        eprint!("{line}");
    }
    #[cfg(not(test))]
    let _ = io::stderr().write_all(line.as_bytes());
}
/// The backstop under every message, for text that no call site cleaned, such as what a library
/// quotes from the input in its error. One message stays on one line, and can neither drive a
/// terminal nor read as markup.
fn sanitized(text: &str) -> String {
    let mut chars = text.chars();
    let mut out: String = chars
        .by_ref()
        .take(MAX_LINE)
        .map(|c| if is_hostile(c) { '?' } else { c })
        .collect();
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}
/// Control characters, markup, and the invisible format characters that reorder or hide text.
fn is_hostile(c: char) -> bool {
    c.is_control()
        || matches!(c, '<' | '>' | '&')
        || matches!(c as u32, 0xAD | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0xFEFF)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_message_stays_as_written() {
        let message = "Warning: skipped 3 faces; use --center (bounds 0 → 1.5) `#VRML` it's";
        assert_eq!(sanitized(message), message);
    }
    #[test]
    fn nothing_can_drive_a_terminal_or_read_as_markup() {
        assert_eq!(sanitized("\x1b[31mred\x07"), "?[31mred?");
        assert_eq!(sanitized("a\nb\r\tc"), "a?b??c");
        assert_eq!(sanitized("<img src=x>&amp;"), "?img src=x??amp;");
        assert_eq!(sanitized("a\u{202e}b\u{200b}c\u{feff}"), "a?b?c?");
    }
    #[test]
    fn a_long_message_is_cut() {
        let cut = sanitized(&"é".repeat(MAX_LINE + 1));
        assert_eq!(cut.chars().count(), MAX_LINE + 3);
        assert!(cut.ends_with("é..."), "{cut}");
        assert_eq!(sanitized(&"x".repeat(MAX_LINE)), "x".repeat(MAX_LINE));
    }
}
