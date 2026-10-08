//! Visible escaping for untrusted text at human-readable terminal boundaries.

use std::borrow::Cow;

/// Visible filename representation that distinguishes invalid encoded bytes
/// from literal escape text. This is display text, not a portable path format.
pub fn escape_filename(name: &std::ffi::OsStr) -> Cow<'_, str> {
    if let Some(text) = name.to_str()
        && !text.chars().any(|c| needs_escape(c) || c == '\\')
    {
        return Cow::Borrowed(text);
    }
    use std::fmt::Write;
    let mut output = String::new();
    for chunk in name.as_encoded_bytes().utf8_chunks() {
        for character in chunk.valid().chars() {
            if needs_escape(character) || character == '\\' {
                output.extend(character.escape_default());
            } else {
                output.push(character);
            }
        }
        for byte in chunk.invalid() {
            write!(&mut output, "\\x{byte:02x}").expect("writing to a string cannot fail");
        }
    }
    Cow::Owned(output)
}

fn needs_escape(character: char) -> bool {
    character.is_control()
        || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Escape control characters and directional formatting without changing ordinary Unicode.
/// This is a display representation, not a reversible path encoding.
pub fn escape_terminal_text(text: &str) -> Cow<'_, str> {
    if !text.chars().any(needs_escape) {
        return Cow::Borrowed(text);
    }
    let mut result = String::with_capacity(text.len());
    for character in text.chars() {
        if needs_escape(character) {
            result.extend(character.escape_default());
        } else {
            result.push(character);
        }
    }
    Cow::Owned(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn filename_labels_distinguish_bytes_controls_and_literal_escapes() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            escape_filename(std::ffi::OsStr::from_bytes(b"bad-\xff")),
            "bad-\\xff"
        );
        assert_eq!(
            escape_filename(std::ffi::OsStr::from_bytes(b"bad-\xfe")),
            "bad-\\xfe"
        );
        assert_eq!(
            escape_filename(std::ffi::OsStr::new(r"bad-\xff")),
            r"bad-\\xff"
        );
        assert_eq!(escape_filename(std::ffi::OsStr::new("line\n")), r"line\n");
        assert_eq!(escape_filename(std::ffi::OsStr::new(r"line\n")), r"line\\n");
        assert!(matches!(
            escape_filename(std::ffi::OsStr::new("résumé 日本語")),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn controls_and_direction_overrides_are_visible_not_executable() {
        assert_eq!(
            escape_terminal_text("a\n\t\r\u{1b}]52;c;secret\u{7}\u{202e}txt"),
            "a\\n\\t\\r\\u{1b}]52;c;secret\\u{7}\\u{202e}txt"
        );
        assert!(
            escape_terminal_text("\u{85}\u{2028}\u{2067}")
                .chars()
                .all(|ch| !needs_escape(ch))
        );
    }

    #[test]
    fn ordinary_unicode_is_preserved_without_allocation() {
        assert!(matches!(
            escape_terminal_text("résumé 日本語 📁"),
            Cow::Borrowed("résumé 日本語 📁")
        ));
    }
}
