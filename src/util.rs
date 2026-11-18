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

/// Containment and extension of LSP ranges using their half-open boundaries.
pub trait TextRangeExt {
    /// Tests half-open containment of an LSP UTF-16 position.
    fn contains_position(&self, position: Position) -> bool;

    /// Tests closed containment: a cursor just after a word still touches it.
    fn touches(&self, position: Position) -> bool;

    /// The range from this range's start to `last`'s end: an assignment's
    /// target `k` through its value `delta - 1` gives the whole `k := delta - 1`.
    fn through(&self, last: TextRange) -> TextRange;
}

impl TextRangeExt for TextRange {
    fn contains_position(&self, position: Position) -> bool {
        self.start <= position && position < self.end
    }

    fn touches(&self, position: Position) -> bool {
        self.start <= position && position <= self.end
    }

    fn through(&self, last: TextRange) -> TextRange {
        TextRange::new(self.start, last.end)
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
