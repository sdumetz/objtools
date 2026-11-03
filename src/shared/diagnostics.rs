//! Everything objtools writes to stderr. A host may show it to whoever supplied the file, so it
//! has to be safe to show as it stands, with nothing left for the host to strip.
#[cfg(not(test))]
use std::io::{self, Write};

/// Long enough for any message objtools composes. Text from a file is capped well below this.
const MAX_LINE: usize = 512;
const MAX_TEXT: usize = 64;

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
/// Text from a file, for use inside a message: names, keywords and references. Cleaned like any
/// message, and cut so that a message stays short whatever the file says.
pub fn safe(text: &str) -> String {
    let mut out: String = text.chars().take(MAX_TEXT).map(cleaned).collect();
    if text.chars().nth(MAX_TEXT).is_some() {
        let keep = out
            .char_indices()
            .nth(MAX_TEXT - 3)
            .map_or(out.len(), |(i, _)| i);
        out.truncate(keep);
        out.push_str("...");
    }
    out
}
/// The backstop under every message, for text that no call site cleaned, such as what a library
/// quotes from the input in its error. One message stays on one line, and can neither drive a
/// terminal nor read as markup.
fn sanitized(text: &str) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(MAX_LINE).map(cleaned).collect();
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}
/// Control characters, markup, and the invisible format characters that reorder or hide text.
fn cleaned(c: char) -> char {
    let hostile = c.is_control()
        || matches!(c, '<' | '>' | '&')
        || matches!(c as u32, 0xAD | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0xFEFF);
    if hostile {
        '?'
    } else {
        c
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_names_stay_as_written() {
        for name in [
            "wood_01.png",
            "textures/Mur (2).JPG",
            "m,1:a-b",
            "été 木",
            "l'église [v2]+#1.obj",
        ] {
            assert_eq!(safe(name), name);
        }
    }
    #[test]
    fn what_could_do_harm_in_a_name_is_replaced() {
        assert_eq!(safe("\x1b[31m0"), "?[31m0");
        assert_eq!(safe("<b>a&b\n"), "?b?a?b?");
        assert_eq!(safe("a\u{202e}b"), "a?b");
    }
    #[test]
    fn a_long_name_is_cut_to_the_cap() {
        let name = "é".repeat(MAX_TEXT);
        assert_eq!(safe(&name), name);
        let cut = safe(&"é".repeat(MAX_TEXT + 1));
        assert_eq!(cut.chars().count(), MAX_TEXT);
        assert!(cut.ends_with("é..."), "{cut}");
    }
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
