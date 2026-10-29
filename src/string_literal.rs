//! Macaulay2's two string literal forms, read into and written from the
//! strings they denote.

use m2_syn::nodes::{RawStringLiteral, StringLiteral};

use crate::node_metadata::SyntaxNode;

/// The radix of an escape writing a byte by octal code, `\101`.
const OCTAL_RADIX: u32 = 8;
/// The radix of an escape writing a byte by hexadecimal code, `\x41`.
const HEX_RADIX: u32 = 16;
/// The most digits an octal escape takes, `\101`.
const OCTAL_ESCAPE_DIGITS: usize = 3;
/// The most digits a hexadecimal escape takes, `\x41`.
const HEX_ESCAPE_DIGITS: usize = 2;
/// The delimiter of a verbatim string, `///…///`, on both sides.
const TRIPLE_SLASH: &str = "///";

/// How a string literal delimits its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StringDelimiter {
    /// `"…"`: backslash escapes stand for the characters they name.
    Quote,
    /// `///…///`: the text is verbatim and ends at the first `///`.
    TripleSlash,
}

impl StringDelimiter {
    /// The delimiter of the string literal `node`.
    pub fn of(node: SyntaxNode<'_>) -> Option<Self> {
        if node.is::<StringLiteral>() {
            Some(Self::Quote)
        } else if node.is::<RawStringLiteral>() {
            Some(Self::TripleSlash)
        } else {
            None
        }
    }

    /// The string `content`, written between these delimiters, denotes.
    /// `None` for an escape Macaulay2 does not read, or one writing bytes
    /// that are not UTF-8.
    pub fn decode(self, content: &str) -> Option<String> {
        if self == Self::TripleSlash {
            return Some(content.to_string());
        }
        let mut bytes = Vec::with_capacity(content.len());
        let mut chars = content.chars().peekable();
        while let Some(char) = chars.next() {
            if char != '\\' {
                bytes.extend_from_slice(char.encode_utf8(&mut [0; 4]).as_bytes());
                continue;
            }
            let escaped = chars.next()?;
            let byte = match escaped {
                '\\' => b'\\',
                '"' => b'"',
                'n' => b'\n',
                'r' => b'\r',
                't' => b'\t',
                'a' => 0x07,
                'b' => 0x08,
                'f' => 0x0c,
                'v' => 0x0b,
                'e' => 0x1b,
                'x' => Self::escaped_byte(&mut chars, HEX_RADIX, HEX_ESCAPE_DIGITS, None)?,
                digit if digit.is_digit(OCTAL_RADIX) => Self::escaped_byte(
                    &mut chars,
                    OCTAL_RADIX,
                    OCTAL_ESCAPE_DIGITS - 1,
                    Some(digit),
                )?,
                _ => return None,
            };
            bytes.push(byte);
        }
        String::from_utf8(bytes).ok()
    }

    /// The byte of the digits in `radix` an escape writes: `first`, when
    /// already read, then up to `digits` more from `chars`.
    fn escaped_byte(
        chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
        radix: u32,
        digits: usize,
        first: Option<char>,
    ) -> Option<u8> {
        let mut value = first.and_then(|digit| digit.to_digit(radix));
        for _ in 0..digits {
            let Some(digit) = chars.peek().and_then(|digit| digit.to_digit(radix)) else {
                break;
            };
            chars.next();
            value = Some(value.unwrap_or(0) * radix + digit);
        }
        u8::try_from(value?).ok()
    }

    /// `value` written between these delimiters, without them. `None` when
    /// they cannot hold it: a verbatim string cannot contain the `///`
    /// ending it, nor end in a `/` that would join its closing delimiter.
    pub fn encode(self, value: &str) -> Option<String> {
        match self {
            Self::Quote => Some(
                value
                    .chars()
                    .map(|char| match char {
                        '\\' => "\\\\".to_string(),
                        '"' => "\\\"".to_string(),
                        '\n' => "\\n".to_string(),
                        '\t' => "\\t".to_string(),
                        control if control.is_ascii_control() => {
                            format!("\\{:03o}", u32::from(control))
                        }
                        char => char.to_string(),
                    })
                    .collect(),
            ),
            Self::TripleSlash => (format!("{value}{TRIPLE_SLASH}").find(TRIPLE_SLASH)
                == Some(value.len()))
            .then(|| value.to_string()),
        }
    }

    /// `value` as a whole literal with these delimiters.
    pub fn literal(self, value: &str) -> Option<String> {
        let content = self.encode(value)?;
        Some(match self {
            Self::Quote => format!("\"{content}\""),
            Self::TripleSlash => format!("{TRIPLE_SLASH}{content}{TRIPLE_SLASH}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::StringDelimiter::{Quote, TripleSlash};

    #[test]
    fn decoding_reads_every_escape_macaulay2_reads() {
        assert_eq!(
            Quote.decode(r#"we\"ird\\.m2"#).as_deref(),
            Some(r#"we"ird\.m2"#)
        );
        assert_eq!(Quote.decode(r"\101\x41\7").as_deref(), Some("AA\u{7}"));
        assert_eq!(
            Quote.decode(r"\a\b\f\v\e").as_deref(),
            Some("\u{7}\u{8}\u{c}\u{b}\u{1b}")
        );
        assert_eq!(Quote.decode(r"\q"), None, "not an escape");
        assert_eq!(Quote.decode(r"\377"), None, "not UTF-8");
        assert_eq!(TripleSlash.decode(r"a\nb").as_deref(), Some(r"a\nb"));
    }

    #[test]
    fn encoding_round_trips_or_refuses() {
        for value in ["plain.m2", "we\"ird\\.m2", "tab\there", "bell\u{7}9"] {
            let encoded = Quote.encode(value).expect("a quoted string holds anything");
            assert_eq!(Quote.decode(&encoded).as_deref(), Some(value), "{encoded}");
        }
        assert_eq!(
            TripleSlash.literal("a b.m2").as_deref(),
            Some("///a b.m2///")
        );
        assert_eq!(TripleSlash.encode("a///b"), None);
        assert_eq!(TripleSlash.encode("dir/"), None);
    }
}
