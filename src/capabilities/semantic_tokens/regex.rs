//! Source-mapped highlighting of syntax inside regular-expression literals.

use crate::semantic_token::M2SemanticTokenType;
use crate::source::ByteRange;
use crate::string_literal::StringDelimiter;

/// Regex punctuation outside bracket expressions.
const METACHARACTERS: &str = ".^$*+?{}()|[]";
/// Delimiter of a verbatim Macaulay2 string.
const RAW_DELIMITER: &str = "///";

/// A decoded pattern character and the source spelling that produced it.
struct PatternCharacter {
    /// Character seen by the regular-expression engine.
    value: char,
    /// Original source bytes, including any string escape.
    bytes: ByteRange,
    /// Whether Macaulay2 decodes this character from an escape.
    string_escape: bool,
}

/// Highlighted runs within a complete literal, preserving original byte offsets.
pub(super) struct RegexTokens {
    /// Decoded characters retaining their original string spellings.
    characters: Vec<PatternCharacter>,
    /// Full source extent, including delimiters.
    bytes: ByteRange,
}

impl RegexTokens {
    /// Reads the spelling of a string already identified as a pattern by analysis.
    pub(super) fn new(text: &str, bytes: ByteRange) -> Self {
        let raw = text.starts_with(RAW_DELIMITER);
        let width = if raw { RAW_DELIMITER.len() } else { 1 };
        let mut characters = Vec::new();
        let mut offset = width;
        let end = text.len().saturating_sub(width);
        while offset < end {
            let start = offset;
            let Some(value) = text[offset..end].chars().next() else {
                break;
            };
            offset += value.len_utf8();
            let string_escape = !raw && value == '\\';
            let value = if string_escape && offset < end {
                let escaped = text[offset..end].chars().next().unwrap_or(value);
                offset += escaped.len_utf8();
                let (radix, limit) = match escaped {
                    'x' => (16, 2),
                    '0'..='7' => (8, 2),
                    _ => (0, 0),
                };
                for _ in 0..limit {
                    if text[offset..end]
                        .chars()
                        .next()
                        .is_some_and(|digit| digit.is_digit(radix))
                    {
                        offset += 1;
                    } else {
                        break;
                    }
                }
                StringDelimiter::Quote
                    .decode(&text[start..offset])
                    .and_then(|decoded| decoded.chars().next())
                    .unwrap_or(escaped)
            } else {
                value
            };
            characters.push(PatternCharacter {
                value,
                bytes: bytes.start + start..bytes.start + offset,
                string_escape,
            });
        }
        Self { characters, bytes }
    }

    /// Partitions the literal into strings, regex punctuation, and escape sequences.
    /// Bracket contents are text except for their delimiters, ranges, and escapes.
    pub(super) fn runs(&self) -> Vec<(ByteRange, M2SemanticTokenType)> {
        let mut runs = Vec::new();
        let mut cursor = self.bytes.start;
        let mut in_class = false;
        let mut class_start = false;
        let mut index = 0;
        while index < self.characters.len() {
            let character = &self.characters[index];
            if cursor < character.bytes.start {
                runs.push((cursor..character.bytes.start, M2SemanticTokenType::String));
            }
            let mut bytes = character.bytes.clone();
            let kind = if character.value == '\\' {
                let end = self.escape_end(index);
                bytes.end = self.characters[end].bytes.end;
                index = end;
                class_start = false;
                M2SemanticTokenType::EscapeSequence
            } else {
                let special = if in_class {
                    match character.value {
                        ']' if !class_start => {
                            in_class = false;
                            true
                        }
                        '^' if class_start => true,
                        '-' => {
                            !class_start
                                && self
                                    .characters
                                    .get(index + 1)
                                    .is_some_and(|next| next.value != ']')
                        }
                        _ => false,
                    }
                } else {
                    if character.value == '[' {
                        in_class = true;
                        class_start = true;
                    }
                    METACHARACTERS.contains(character.value)
                };
                if character.value != '[' && character.value != '^' {
                    class_start = false;
                }
                if character.string_escape {
                    M2SemanticTokenType::EscapeSequence
                } else if special {
                    M2SemanticTokenType::Regexp
                } else {
                    M2SemanticTokenType::String
                }
            };
            cursor = bytes.end;
            runs.push((bytes, kind));
            index += 1;
        }
        if cursor < self.bytes.end {
            runs.push((cursor..self.bytes.end, M2SemanticTokenType::String));
        }
        let mut merged: Vec<(ByteRange, M2SemanticTokenType)> = Vec::new();
        for (bytes, kind) in runs {
            if let Some((previous, previous_kind)) = merged.last_mut() {
                if previous.end == bytes.start && *previous_kind == kind {
                    previous.end = bytes.end;
                    continue;
                }
            }
            merged.push((bytes, kind));
        }
        merged
    }

    /// Last character of a regex escape, including hexadecimal and named forms.
    fn escape_end(&self, start: usize) -> usize {
        let Some(escaped) = self.characters.get(start + 1) else {
            return start;
        };
        let mut end = start + 1;
        if matches!(escaped.value, 'x' | 'p' | 'P' | 'N')
            && self
                .characters
                .get(end + 1)
                .is_some_and(|next| next.value == '{')
        {
            if let Some(close) = self.characters[end + 2..]
                .iter()
                .position(|next| next.value == '}')
            {
                return end + 2 + close;
            }
        }
        let digits = match escaped.value {
            'x' => 2,
            'u' => 4,
            'U' => 8,
            _ => 0,
        };
        for _ in 0..digits {
            if self
                .characters
                .get(end + 1)
                .is_some_and(|next| next.value.is_ascii_hexdigit())
            {
                end += 1;
            } else {
                break;
            }
        }
        end
    }
}
