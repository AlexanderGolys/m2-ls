//! Naming conventions shared by declaration and option diagnostics.

/// Whether a name consists of ASCII letters and digits without separators,
/// and starts with the requested case. Leading underscores mark unused locals.
pub(super) fn follows_case(name: &str, uppercase: bool) -> bool {
    let name = name.trim_start_matches('_');
    let mut characters = name.chars();
    characters.next().is_some_and(|first| {
        if uppercase {
            first.is_ascii_uppercase()
        } else {
            first.is_ascii_lowercase()
        }
    }) && characters.all(|character| character.is_ascii_alphanumeric())
}
