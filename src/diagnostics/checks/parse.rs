//! Diagnostics retained by the native lexer and parser for one source cell.

use m2_syn::NativeParseError;

use super::DiagnosticSink;
use crate::diagnostics::{catalog, Finding};
use crate::node_metadata::ParsedCell;
use crate::source::SourceNavigation;

/// Publishes a cell's parser and lexer failures before its semantic checks.
pub(super) fn spawn(
    cell: &ParsedCell,
    source: &(impl SourceNavigation + ?Sized),
    sink: &mut DiagnosticSink,
) {
    if matches!(
        cell.parse_error(),
        Some(NativeParseError::InvalidAssignmentTarget { .. })
    ) {
        sink.spawn(
            &catalog::MULTIPLE_ASSIGNMENT_TARGETS,
            Finding::new(
                source.range_for_bytes(cell.document_range()),
                "This parallel assignment contains a target that cannot be assigned",
            ),
        );
        return;
    }
    for bytes in cell.error_ranges() {
        sink.spawn(
            &catalog::SYNTAX_ERROR,
            Finding::new(source.range_for_bytes(bytes), "Syntax error"),
        );
    }
}
