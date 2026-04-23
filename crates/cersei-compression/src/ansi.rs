//! ANSI escape handling.
//!
//! Credits: adapted from rtk (Rust Token Killer) — `rtk/src/core/utils.rs`.
//! MIT © Patrick Szymkowiak. See LICENSE.

use once_cell::sync::Lazy;
use regex::Regex;

static ANSI_RE: Lazy<Regex> = Lazy::new(|| {
    // Covers CSI sequences (color/style) and common OSC/ESC codes.
    Regex::new(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)").unwrap()
});

pub fn strip_ansi(text: &str) -> String {
    ANSI_RE.replace_all(text, "").into_owned()
}

/// Unicode-safe char truncation — keeps at most `max_len` chars, appends `...`
/// when the input is longer.
pub fn truncate(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        s.to_string()
    } else if max_len < 3 {
        "...".to_string()
    } else {
        format!("{}...", s.chars().take(max_len - 3).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_basic() {
        assert_eq!(strip_ansi("\x1b[31mError\x1b[0m"), "Error");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\x1b[1m\x1b[32mOK\x1b[0m\x1b[0m"), "OK");
    }

    #[test]
    fn truncate_unicode() {
        assert_eq!(truncate("hello world", 8), "hello...");
        assert_eq!(truncate("hi", 10), "hi");
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("hello world", 3), "...");
        // Thai, multi-byte
        assert_eq!(truncate("สวัสดีครับ", 5).chars().count(), 5);
    }
}
