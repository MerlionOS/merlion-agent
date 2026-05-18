//! Multiline editable input buffer with history navigation.
//!
//! The TUI's input bar uses the editing helpers on `super::app::App`
//! directly (insert_char, backspace, navigate_history). This module is
//! reserved for future widget extraction — e.g. a richer editor with
//! word-wise navigation or syntax-highlighted slash commands.
//!
//! For now it exists so the `tui` module mirrors the layout described in
//! the design doc (`mod app; mod render; mod input;`) and so that future
//! growth doesn't require refactoring app.rs.

/// Return the byte index of the start of the previous grapheme-ish unit
/// (current implementation: the previous char boundary).
#[allow(dead_code)]
pub fn prev_char_boundary(s: &str, byte_pos: usize) -> usize {
    if byte_pos == 0 {
        return 0;
    }
    let mut p = byte_pos - 1;
    while !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

/// Return the byte index of the end of the current grapheme-ish unit.
#[allow(dead_code)]
pub fn next_char_boundary(s: &str, byte_pos: usize) -> usize {
    if byte_pos >= s.len() {
        return s.len();
    }
    let mut p = byte_pos + 1;
    while p < s.len() && !s.is_char_boundary(p) {
        p += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prev_at_zero_stays_zero() {
        assert_eq!(prev_char_boundary("abc", 0), 0);
    }

    #[test]
    fn next_at_end_stays_end() {
        let s = "abc";
        assert_eq!(next_char_boundary(s, s.len()), s.len());
    }

    #[test]
    fn handles_multibyte() {
        // "é" is 2 bytes in UTF-8.
        let s = "é";
        assert_eq!(prev_char_boundary(s, s.len()), 0);
        assert_eq!(next_char_boundary(s, 0), s.len());
    }
}
