//! UTF-16 code unit offset <-> byte offset conversion.
//!
//! `EntityInputHandler` deals in UTF-16 code unit offsets into the text the
//! OS was told about; every `TextSource` here deals in byte offsets. These
//! free functions bridge the two at the `gpui` boundary -- they stay out of
//! `TextSource` itself so a text source has no reason to know what an OS
//! text input API counts in.

use std::ops::Range;

#[must_use]
pub fn byte_offset_from_utf16(text: &str, offset_utf16: usize) -> usize {
    let mut utf16_count = 0;
    for (byte_idx, ch) in text.char_indices() {
        if utf16_count >= offset_utf16 {
            return byte_idx;
        }
        utf16_count += ch.len_utf16();
    }
    text.len()
}

#[must_use]
pub fn byte_offset_to_utf16(text: &str, offset: usize) -> usize {
    text[..offset].chars().map(char::len_utf16).sum()
}

#[must_use]
pub fn byte_range_from_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    byte_offset_from_utf16(text, range.start)..byte_offset_from_utf16(text, range.end)
}

#[must_use]
pub fn byte_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    byte_offset_to_utf16(text, range.start)..byte_offset_to_utf16(text, range.end)
}

#[cfg(test)]
mod tests {
    use super::{
        byte_offset_from_utf16, byte_offset_to_utf16, byte_range_from_utf16, byte_range_to_utf16,
    };

    #[test]
    fn ascii_utf16_offsets_match_byte_offsets() {
        let text = "hello world";
        assert_eq!(byte_offset_from_utf16(text, 5), 5);
        assert_eq!(byte_offset_to_utf16(text, 5), 5);
        assert_eq!(byte_range_from_utf16(text, 0..5), 0..5);
        assert_eq!(byte_range_to_utf16(text, 0..5), 0..5);
    }

    #[test]
    fn utf16_offsets_round_trip_through_a_surrogate_pair() {
        // U+1F600 sits outside the BMP: one `char`, four UTF-8 bytes, two
        // UTF-16 code units -- exactly the case a naive byte-count or
        // char-count implementation of the UTF-16 boundary math gets wrong.
        let text = "a\u{1F600}b";
        assert_eq!(byte_offset_from_utf16(text, 1), 1, "just after 'a'");
        assert_eq!(
            byte_offset_from_utf16(text, 3),
            5,
            "past both UTF-16 units of the emoji, at 'b'"
        );
        assert_eq!(byte_offset_to_utf16(text, 1), 1);
        assert_eq!(byte_offset_to_utf16(text, 5), 3);
        assert_eq!(byte_range_to_utf16(text, 1..5), 1..3);
        assert_eq!(byte_range_from_utf16(text, 1..3), 1..5);
    }

    #[test]
    fn an_offset_past_the_end_clamps_to_the_text_length() {
        let text = "abc";
        assert_eq!(byte_offset_from_utf16(text, 99), text.len());
    }

    #[test]
    fn multiline_text_counts_the_newline_as_one_utf16_unit() {
        let text = "ab\ncd";
        assert_eq!(byte_offset_from_utf16(text, 3), 3, "just after the newline");
        assert_eq!(byte_offset_to_utf16(text, 3), 3);
    }
}
