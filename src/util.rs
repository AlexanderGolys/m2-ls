//! Small LSP value predicates shared across capabilities.

use tower_lsp::lsp_types::Range as TextRange;
use tower_lsp::lsp_types::*;

/// Construct an LSP position with concise fixture syntax.
#[macro_export]
macro_rules! pos {
    () => {
        tower_lsp::lsp_types::Position::new(0, 0)
    };
    ($line:expr, $character:expr) => {
        tower_lsp::lsp_types::Position::new($line, $character)
    };
}

/// Construct the sentinel position after every real source position.
#[macro_export]
macro_rules! pos_max {
    () => {
        tower_lsp::lsp_types::Position::new(u32::MAX, u32::MAX)
    };
}

pub trait TextRangeExt {
    fn contains_position(&self, position: Position) -> bool;
    fn is_inside(&self, outer: TextRange) -> bool;
}

impl TextRangeExt for TextRange {
    fn contains_position(&self, position: Position) -> bool {
        if position.line < self.start.line || position.line > self.end.line {
            return false;
        }
        if position.line == self.start.line && position.character < self.start.character {
            return false;
        }
        if position.line == self.end.line && position.character >= self.end.character {
            return false;
        }
        true
    }

    fn is_inside(&self, outer: TextRange) -> bool {
        let starts_inside = self.start.line > outer.start.line
            || (self.start.line == outer.start.line
                && self.start.character >= outer.start.character);
        let ends_inside = self.end.line < outer.end.line
            || (self.end.line == outer.end.line && self.end.character <= outer.end.character);
        starts_inside && ends_inside && *self != outer
    }
}

#[cfg(test)]
mod tests {
    use tower_lsp::lsp_types::Position;

    #[test]
    fn position_macros_construct_requested_sentinels() {
        assert_eq!(pos!(), Position::new(0, 0));
        assert_eq!(pos!(3, 5), Position::new(3, 5));
        assert_eq!(pos_max!(), Position::new(u32::MAX, u32::MAX));
    }
}
