//! Inline values: what an expression evaluates to where the program
//! determines it statically, shown beside it as `S_6 = z`. Values come from
//! the analysis, which records them beyond types; nothing is evaluated by
//! running Macaulay2.

use tower_lsp::lsp_types::{
    InlineValue, InlineValueServerCapabilities, InlineValueText, OneOf, Range,
};

use crate::document::DocumentSnapshot;
use crate::source::SourceNavigation;
use crate::typesystem::TypeKnowledge;

pub fn inline_value_provider_capability() -> Option<OneOf<bool, InlineValueServerCapabilities>> {
    Some(OneOf::Left(true))
}

/// The inline values of the expressions of `document` lying within `range`:
/// each variable of a ring the program writes out that an expression
/// selects, by index `S_6` or by name `"x"_A`.
pub fn inline_values_response(
    document: &DocumentSnapshot,
    range: Range,
    knowledge: &(impl TypeKnowledge + ?Sized),
) -> Vec<InlineValue> {
    let analysis = document.analysis();
    document
        .root_node()
        .descendants()
        .filter_map(|node| {
            // A holder such as `(S_6)` shares the value of the subscript in it.
            if node.is_holder() {
                return None;
            }
            let node_range = document.range_for_node(node);
            if node_range.start < range.start || range.end < node_range.end {
                return None;
            }
            let selected = analysis
                .selected_ring_variable(node, document, knowledge)?
                .ok()?;
            Some(InlineValue::Text(InlineValueText {
                range: node_range,
                text: format!("{} = {}", node.text(), selected.variable),
            }))
        })
        .collect()
}
