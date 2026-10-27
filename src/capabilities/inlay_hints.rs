//! Type inlay hints derived from static document analysis.
//!
//! Each [`HintAnchor`] the settings show picks the values it annotates, and
//! one filter shared by all of them drops the hints that would repeat what
//! the source or a nearby hint already says. Anchors picking the same value
//! give the same hint at the same place, and it is shown once.

use std::collections::HashMap;
use std::iter;

use serde::{Deserialize, Serialize};

use m2_syn::nodes::{ExprLambda as LambdaExpression, ExprNew as NewStatement, Symbol};
use m2_syn::Token;
use tower_lsp::lsp_types::{
    InlayHint, InlayHintKind, InlayHintLabel, InlayHintServerCapabilities, InlayHintTooltip, OneOf,
    Position, Range as TextRange,
};

use crate::document::DocumentSnapshot;
use crate::meta::BindingRole;
use crate::node_metadata::{SyntaxNode, SyntaxNodeId};
use crate::object_registry::ObjectRegistry;
use crate::settings::InlayHintSettings;
use crate::source::SourceNavigation;
use crate::typesystem::InferredType;
use crate::util::TextRangeExt;

/// What a call-result hint starts with, before the type the call returns.
const CALL_RESULT_PREFIX: &str = "-> ";
/// The type of `null`: Macaulay2 prints nothing for a cell of this value.
const NOTHING_TYPE: &str = "Nothing";
/// What ends a label cut short to `maxLabelLength`.
const TRUNCATION_MARK: char = '…';

/// A kind of place types are shown at; `inlayHints.show` lists those shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum HintAnchor {
    /// After a binding's name, where it is first assigned or its type
    /// changes, unless its value states it.
    Bindings,
    /// After the value a function returns.
    Returns,
    /// After each top-level expression Macaulay2 prints a value for, as its
    /// `oN : T` line does: not those ended by `;`, nor those of value `null`.
    Cells,
    /// After calls, with the type they return; `callForm` picks which.
    Calls,
    /// After each closing bracket; one closing a call's arguments gives the
    /// type the call returns.
    Brackets,
    /// After the largest expression ending a line while an expression around
    /// it goes on below, as each line of a chain broken across lines.
    LineEnds,
    /// After each argument of a call.
    Arguments,
    /// After every expression.
    Subexpressions,
}

impl HintAnchor {
    /// The anchors shown unless the settings list others.
    pub const DEFAULT_SHOWN: [Self; 4] =
        [Self::Bindings, Self::Returns, Self::Cells, Self::LineEnds];
}

/// Which calls the [`HintAnchor::Calls`] anchor annotates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum CallForm {
    /// Those whose arguments are in brackets, as in `f(x)` or `QQ[x]`.
    #[default]
    Bracketed,
    /// Also those applied by juxtaposition, as in `f x`.
    All,
}

pub fn inlay_hint_provider_capability() -> Option<OneOf<bool, InlayHintServerCapabilities>> {
    Some(OneOf::Left(true))
}

pub fn inlay_hints_response(
    document: &DocumentSnapshot,
    range: TextRange,
    options: &InlayHintSettings,
    knowledge: &ObjectRegistry,
) -> Vec<InlayHint> {
    let context = HintContext::new(document, knowledge, options, range);
    let mut hints = options
        .show
        .iter()
        .flat_map(|anchor| context.anchored_hints(*anchor))
        .collect::<Vec<_>>();
    hints.sort_by(|left, right| {
        (
            left.position.line,
            left.position.character,
            hint_order(&left.label),
            label_text(&left.label),
        )
            .cmp(&(
                right.position.line,
                right.position.character,
                hint_order(&right.label),
                label_text(&right.label),
            ))
    });
    hints.dedup_by(|left, right| {
        left.position == right.position
            && left.kind == right.kind
            && label_text(&left.label) == label_text(&right.label)
    });

    hints
}

/// What the hints of one request are computed from: the document, the types
/// analysis inferred for its expressions, and the settings choosing which
/// are shown.
struct HintContext<'request, 'tree> {
    document: &'tree DocumentSnapshot,
    knowledge: &'request ObjectRegistry,
    options: &'request InlayHintSettings,
    /// The part of the document the client asks hints for.
    range: TextRange,
    inferred_types: HashMap<SyntaxNodeId, (SyntaxNode<'tree>, InferredType)>,
    /// The parts of the document's assignments, whose types binding hints
    /// show.
    assignment_parts: AssignmentParts,
    /// The values assigned to names that state their own type, other than
    /// functions: nothing in them gets a hint.
    opaque_self_describing_values: Vec<TextRange>,
}

fn hint_order(label: &InlayHintLabel) -> bool {
    label_text(label).starts_with(CALL_RESULT_PREFIX)
}

fn inferred_expression_types<'tree>(
    document: &'tree DocumentSnapshot,
    knowledge: &ObjectRegistry,
) -> HashMap<SyntaxNodeId, (SyntaxNode<'tree>, InferredType)> {
    let mut inferred = HashMap::new();
    document.analysis().for_each_expression_type(
        document.root_node(),
        document.syntax(),
        document,
        knowledge,
        |node, inferred_type| {
            inferred.insert(node.id(), (node, inferred_type));
        },
    );
    inferred
}

impl<'request, 'tree> HintContext<'request, 'tree> {
    fn new(
        document: &'tree DocumentSnapshot,
        knowledge: &'request ObjectRegistry,
        options: &'request InlayHintSettings,
        range: TextRange,
    ) -> Self {
        let mut context = Self {
            document,
            knowledge,
            options,
            range,
            inferred_types: inferred_expression_types(document, knowledge),
            assignment_parts: AssignmentParts::default(),
            opaque_self_describing_values: Vec::new(),
        };
        context.assignment_parts = context.assignment_parts();
        context.opaque_self_describing_values = context
            .self_describing_assignment_value_nodes()
            .into_iter()
            .filter(|node| !parenthesized_value(*node).is::<LambdaExpression>())
            .map(|node| document.range_for_node(node))
            .collect();
        context
    }

    /// The hints `anchor` places.
    fn anchored_hints(&self, anchor: HintAnchor) -> Vec<InlayHint> {
        let typed = || self.inferred_types.values().map(|(node, _)| *node);
        match anchor {
            HintAnchor::Bindings => self.binding_type_hints(),
            HintAnchor::Returns => self.lambda_return_type_hints(),
            HintAnchor::Cells => self.value_hints(self.printed_cell_values()),
            HintAnchor::Calls => self.value_hints(typed().filter(|node| self.is_shown_call(*node))),
            HintAnchor::Brackets => self.value_hints(
                typed()
                    .filter(SyntaxNode::is_delimited_expression)
                    .map(|brackets| applied_call(brackets).unwrap_or(brackets)),
            ),
            HintAnchor::LineEnds => self.value_hints(typed().filter(|node| {
                self.ends_line_of_longer_expression(*node) && !self.ends_typed_ancestor(*node)
            })),
            HintAnchor::Arguments => self.value_hints(
                typed()
                    .filter(|node| node.is_space_application() && !self.is_excluded_call(*node))
                    .flat_map(call_arguments),
            ),
            HintAnchor::Subexpressions => self.value_hints(typed()),
        }
    }

    /// The hints after each of `values` that the filters keep.
    fn value_hints(&self, values: impl IntoIterator<Item = SyntaxNode<'tree>>) -> Vec<InlayHint> {
        values
            .into_iter()
            .filter_map(|value| self.value_hint(value))
            .collect()
    }

    /// The hint after `value`, of its type, unless the settings leave it
    /// out; a call result reads `-> T`.
    fn value_hint(&self, value: SyntaxNode<'tree>) -> Option<InlayHint> {
        let (_, inferred_type) = self.inferred_types.get(&value.id())?;
        let value_range = self.document.range_for_node(value);
        let repeated = || {
            self.assignment_parts
                .suppresses(value_range, value.is_space_application())
        };
        let stated_by_literal = || {
            is_self_describing_value(value)
                || self
                    .opaque_self_describing_values
                    .iter()
                    .any(|opaque| range_contains(*opaque, value_range))
        };
        if !range_contains(self.range, value_range)
            || value.is_muted_statement()
            || self.is_excluded_call(value)
            || self.type_is_stated_by_installation(value)
            || (!self.options.repeated_types && repeated())
            || (!self.options.literal_types && stated_by_literal())
        {
            return None;
        }
        let type_name = self.displayed_type(value_range.start, inferred_type)?;
        Some(self.type_hint(value_range.end, expression_label(value, type_name)))
    }

    /// The values of the top-level expressions Macaulay2 prints: those not
    /// ended by `;`, and not known to be `null`.
    fn printed_cell_values(&self) -> impl Iterator<Item = SyntaxNode<'tree>> + '_ {
        self.document
            .root_node()
            .children()
            .filter(|cell| !cell.is_muted_statement())
            .filter_map(|cell| cell.final_value_child())
            .filter(|value| {
                self.inferred_types
                    .get(&value.id())
                    .is_none_or(|(_, inferred_type)| {
                        inferred_type
                            .single()
                            .is_none_or(|name| name.name() != NOTHING_TYPE)
                    })
            })
    }

    /// Whether `node` is a call [`HintAnchor::Calls`] annotates.
    fn is_shown_call(&self, node: SyntaxNode<'_>) -> bool {
        node.is_space_application()
            && (self.options.call_form == CallForm::All
                || node
                    .binary_parts()
                    .is_some_and(|call| call.right.is_delimited_expression()))
    }

    /// Whether `node` calls a function the settings exclude.
    fn is_excluded_call(&self, node: SyntaxNode<'_>) -> bool {
        node.applied_symbol_name()
            .is_some_and(|name| self.options.excluded_calls.contains(name))
    }

    /// Whether a typed expression around `node` ends where it does, and so
    /// takes a hint placed there.
    fn ends_typed_ancestor(&self, node: SyntaxNode<'_>) -> bool {
        node.ancestors()
            .take_while(|ancestor| ancestor.end_byte() == node.end_byte())
            .any(|ancestor| self.inferred_types.contains_key(&ancestor.id()))
    }

    /// A type hint reading `label` at `position`, cut to `maxLabelLength`
    /// with the whole label in its tooltip.
    fn type_hint(&self, position: Position, label: String) -> InlayHint {
        let limit = self
            .options
            .max_label_length
            .filter(|limit| *limit > 0)
            .map(|limit| limit as usize);
        let (label, tooltip) = match limit {
            Some(limit) if label.chars().count() > limit => (
                label
                    .chars()
                    .take(limit - 1)
                    .chain(iter::once(TRUNCATION_MARK))
                    .collect::<String>(),
                Some(InlayHintTooltip::String(label)),
            ),
            _ => (label, None),
        };
        InlayHint {
            position,
            label: InlayHintLabel::from(label),
            kind: Some(InlayHintKind::TYPE),
            text_edits: None,
            tooltip,
            padding_left: Some(true),
            padding_right: None,
            data: None,
        }
    }

    /// The types functions return, after the value they end with: their
    /// final expression, or what `return` returns.
    fn lambda_return_type_hints(&self) -> Vec<InlayHint> {
        let analysis = self.document.analysis();
        self.inferred_types
            .values()
            .filter_map(|(node, _)| {
                let node = *node;
                if node.is_return_expr() {
                    let value = node.control_transfer_value()?;
                    let view = self.knowledge.at(self.document.position_for_node(value));
                    analysis.control_transfer_target(node, self.document, &view)?;
                    Some(value)
                } else if node.is::<LambdaExpression>() {
                    lambda_final_value(node)
                } else {
                    None
                }
            })
            .filter(|value| self.options.literal_types || !is_self_describing_value(*value))
            .filter_map(|value| {
                let value_range = self.document.range_for_node(value);
                if !self.range.contains_position(value_range.end) {
                    return None;
                }
                let type_name = self.inferred_type_label(value)?;
                Some(self.type_hint(value_range.end, expression_label(value, type_name)))
            })
            .collect()
    }

    /// The label of the inferred type of `node`, when it is shown.
    fn inferred_type_label(&self, node: SyntaxNode<'_>) -> Option<String> {
        self.displayed_type(
            self.document.position_for_node(node),
            &self.inferred_types.get(&node.id())?.1,
        )
    }

    /// The label of `inferred_type`, the type of a value at `position`; none
    /// for a type that says nothing unless the settings ask for those.
    fn displayed_type(&self, position: Position, inferred_type: &InferredType) -> Option<String> {
        if (inferred_type.is_unknown() && !self.options.unknown_types)
            || self.is_hidden(inferred_type)
        {
            return None;
        }
        let view = self.knowledge.at(position);
        inferred_type
            .subset_label(|generator| {
                self.document
                    .analysis()
                    .has_strict_subtype_at(generator, position, &view)
            })
            .map(inlay_type_label)
    }

    /// Whether every type `inferred_type` ranges over is one the settings
    /// hide.
    fn is_hidden(&self, inferred_type: &InferredType) -> bool {
        let mut members = inferred_type
            .exact_points()
            .chain(inferred_type.upward_generators())
            .peekable();
        members.peek().is_some() && members.all(|member| self.options.hidden_types.contains(member))
    }

    /// The types of bindings, after their names: where a binding is first
    /// assigned or its type changes, unless the value states it.
    fn binding_type_hints(&self) -> Vec<InlayHint> {
        let assigned_values = self.assigned_values();
        let self_describing_values = self
            .self_describing_assignment_value_nodes()
            .into_iter()
            .map(|node| self.document.range_for_node(node))
            .collect::<Vec<_>>();
        let mut hints = Vec::new();

        for binding in self.document.analysis().bindings() {
            let mut previous_type: Option<String> = None;
            for state in &binding.states {
                if binding.role == BindingRole::Parameter && state.inferred_type.is_some() {
                    continue;
                }
                let assigned = assigned_values
                    .iter()
                    .find(|value| value.target_range == state.span);
                let type_name = assigned.map(|value| value.type_name.clone()).or_else(|| {
                    state.inferred_type.as_ref().and_then(|inferred_type| {
                        self.displayed_type(state.span.start, inferred_type)
                    })
                });
                let Some(type_name) = type_name else {
                    previous_type = None;
                    continue;
                };
                let changed = previous_type.as_ref() != Some(&type_name);
                previous_type = Some(type_name.clone());
                if !self.options.repeated_types
                    && !changed
                    && assigned.is_none_or(|value| !value.destructured)
                {
                    continue;
                }
                let value_range = assigned
                    .map(|value| value.value_range)
                    .or(state.value_range);
                if !self.options.literal_types
                    && value_range.is_some_and(|value_range| {
                        self_describing_values.contains(&value_range)
                            && assigned.is_none_or(|value| !value.destructured)
                    })
                {
                    continue;
                }
                let position = state.span.end;
                if self.range.contains_position(position) {
                    hints.push(self.type_hint(position, type_name));
                }
            }
        }

        hints
    }

    /// Whether `node` ends its line — nothing but a comment follows it there —
    /// while an expression around it goes on below, as a link of a chain
    /// broken across lines does.
    fn ends_line_of_longer_expression(&self, node: SyntaxNode<'_>) -> bool {
        let text = self.document.text();
        let rest_of_line = text[node.end_byte()..]
            .split('\n')
            .next()
            .unwrap_or_default();
        let next_token = node.end_byte() + rest_of_line.len() - rest_of_line.trim_start().len();
        let ends_line = rest_of_line.trim().is_empty()
            || self
                .document
                .node_at_position_minimal(
                    self.document.range_for_bytes(next_token..next_token).start,
                )
                .is_some_and(|following| following.is_comment());
        let line = self.document.range_for_node(node).end.line;
        ends_line
            && node
                .ancestors()
                .take_while(|ancestor| !ancestor.is_source_cell())
                .any(|ancestor| self.document.range_for_node(ancestor).end.line > line)
    }

    /// Whether `node` names a parameter whose type its method installation
    /// states.
    fn type_is_stated_by_installation(&self, node: SyntaxNode<'_>) -> bool {
        node.is::<Symbol>()
            && self
                .document
                .analysis()
                .get_binding_at(node.text(), self.document.position_for_node(node))
                .is_some_and(|binding| {
                    binding.role == BindingRole::Parameter && binding.state.inferred_type.is_some()
                })
    }

    fn assignment_parts(&self) -> AssignmentParts {
        let mut parts = AssignmentParts {
            assignments: Vec::new(),
            targets: Vec::new(),
            values: Vec::new(),
        };
        for assignment in self
            .inferred_types
            .values()
            .map(|(node, _)| *node)
            .filter(SyntaxNode::is_assignment)
        {
            parts
                .assignments
                .push(self.document.range_for_node(assignment));
            if let Some(target) = assignment.binary_parts().map(|parts| parts.left) {
                parts.targets.push(self.document.range_for_node(target));
            }
            if let Some(value) = assignment.binary_parts().map(|parts| parts.right) {
                parts.values.push(self.document.range_for_node(value));
            }
        }
        parts
    }

    fn assigned_values(&self) -> Vec<AssignedValue> {
        let mut values = Vec::new();
        for assignment in self
            .inferred_types
            .values()
            .map(|(node, _)| *node)
            .filter(|node| {
                node.has_binary_operator::<Token![=]>() || node.has_binary_operator::<Token![:=]>()
            })
        {
            let (Some(left), Some(right)) = (
                assignment.binary_parts().map(|parts| parts.left),
                assignment.binary_parts().map(|parts| parts.right),
            ) else {
                continue;
            };
            let destructured = left.is_collection_expression();
            let pairs = if destructured {
                paired_assignment_values(left, right)
            } else if left.is::<Symbol>() {
                vec![(left, right)]
            } else {
                Vec::new()
            };
            for (target, value) in pairs {
                let Some(type_name) = self.inferred_type_label(value) else {
                    continue;
                };
                values.push(AssignedValue {
                    target_range: self.document.range_for_node(target),
                    value_range: self.document.range_for_node(value),
                    type_name,
                    destructured,
                });
            }
        }
        values
    }

    fn self_describing_assignment_value_nodes(&self) -> Vec<SyntaxNode<'tree>> {
        self.inferred_types
            .values()
            .map(|(node, _)| *node)
            .filter(|node| node.is_assignment())
            .filter_map(|assignment| {
                let left = assignment.binary_parts().map(|parts| parts.left)?;
                let right = assignment.binary_parts().map(|parts| parts.right)?;
                (left.is::<Symbol>() && is_self_describing_value(right)).then_some(right)
            })
            .collect()
    }
}

fn lambda_final_value(lambda: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let mut value = lambda.lambda_body()?;
    while value.is_holder() {
        value = value.final_value_child()?;
    }
    (!value.is_return_expr()).then_some(value)
}

fn label_text(label: &InlayHintLabel) -> &str {
    match label {
        InlayHintLabel::String(text) => text,
        InlayHintLabel::LabelParts(parts) => {
            parts.first().map(|part| part.value.as_str()).unwrap_or("")
        }
    }
}

fn inlay_type_label(type_name: String) -> String {
    let members = type_name.split(" | ").collect::<Vec<_>>();
    match members.as_slice() {
        ["Nothing", value] | [value, "Nothing"] => format!("{value}?"),
        _ => type_name,
    }
}

/// The hint label of `type_name`, the type of `expression`: a call result
/// reads `-> T`.
fn expression_label(expression: SyntaxNode<'_>, type_name: String) -> String {
    if expression.is_space_application() {
        format!("{CALL_RESULT_PREFIX}{type_name}")
    } else {
        type_name
    }
}

/// The call `arguments` are the bracketed arguments of.
fn applied_call(arguments: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    arguments.parent().filter(|call| {
        call.is_space_application()
            && call
                .binary_parts()
                .is_some_and(|parts| parts.right.id() == arguments.id())
    })
}

/// The arguments `call` applies its function to: the elements of a
/// sequence, else the one value.
fn call_arguments(call: SyntaxNode<'_>) -> Vec<SyntaxNode<'_>> {
    let Some(argument) = call
        .binary_parts()
        .map(|parts| parenthesized_value(parts.right))
    else {
        return Vec::new();
    };
    if argument.is_sequence() {
        argument.collection_elements().collect()
    } else {
        vec![argument]
    }
}

struct AssignedValue {
    target_range: TextRange,
    value_range: TextRange,
    type_name: String,
    destructured: bool,
}

#[derive(Default)]
struct AssignmentParts {
    assignments: Vec<TextRange>,
    targets: Vec<TextRange>,
    values: Vec<TextRange>,
}

impl AssignmentParts {
    fn suppresses(&self, expression: TextRange, is_call: bool) -> bool {
        self.assignments.contains(&expression)
            || (!is_call && self.values.contains(&expression))
            || self
                .targets
                .iter()
                .any(|target| range_contains(*target, expression))
    }
}

fn paired_assignment_values<'tree>(
    target: SyntaxNode<'tree>,
    value: SyntaxNode<'tree>,
) -> Vec<(SyntaxNode<'tree>, SyntaxNode<'tree>)> {
    if target.is::<Symbol>() {
        return vec![(target, value)];
    }
    if !target.is_collection_expression() {
        return Vec::new();
    }
    let targets = target.collection_elements().collect::<Vec<_>>();
    let value = parenthesized_value(value);
    if value.is_collection_expression() {
        let values = value.collection_elements().collect::<Vec<_>>();
        if targets.len() != values.len() {
            return Vec::new();
        }
        return targets
            .into_iter()
            .zip(values)
            .flat_map(|(target, value)| paired_assignment_values(target, value))
            .collect();
    }
    match targets.as_slice() {
        [target] => paired_assignment_values(*target, value),
        _ => Vec::new(),
    }
}

fn is_self_describing_value(node: SyntaxNode<'_>) -> bool {
    let node = parenthesized_value(node);
    node.is_literal()
        || node.is_nothing_value()
        || (node.is::<Symbol>() && matches!(node.text(), "true" | "false" | "null"))
        || node.is::<LambdaExpression>()
        || node.is::<NewStatement>()
        || ((node.is_collection_expression() || node.is_sequence())
            && node.collection_elements().all(is_self_describing_value))
}

fn parenthesized_value(mut node: SyntaxNode<'_>) -> SyntaxNode<'_> {
    while node.is_holder() {
        let Some(value) = node.final_value_child() else {
            break;
        };
        node = value;
    }
    node
}

fn range_contains(outer: TextRange, inner: TextRange) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::object_registry::{ObjectName, ObjectRegistry};

    /// The settings showing only `anchors`, with every type that says
    /// something.
    fn showing(anchors: &[HintAnchor]) -> InlayHintSettings {
        InlayHintSettings {
            show: anchors.iter().copied().collect(),
            ..InlayHintSettings::default()
        }
    }

    fn names(names: &[&str]) -> HashSet<ObjectName> {
        names.iter().copied().map(ObjectName::new).collect()
    }

    fn hints_with(
        text: &str,
        options: &InlayHintSettings,
        registry: &ObjectRegistry,
    ) -> Vec<InlayHint> {
        let document =
            DocumentSnapshot::from_text(text.to_string(), registry).expect("fixture should parse");
        let range = TextRange::new(pos!(), pos!(u32::MAX, 0));
        inlay_hints_response(&document, range, options, registry)
    }

    /// The default settings' hints of `text`, with those of unknown types,
    /// and with every subexpression's when `subexpressions` is set.
    fn hints(text: &str, subexpressions: bool) -> Vec<InlayHint> {
        let mut options = InlayHintSettings {
            unknown_types: true,
            ..InlayHintSettings::default()
        };
        if subexpressions {
            options.show.insert(HintAnchor::Subexpressions);
        }
        hints_with(text, &options, &ObjectRegistry::default())
    }

    /// The hints of `text` under `options`, with the library's types known,
    /// as their positions and labels.
    fn library_hints(text: &str, options: &InlayHintSettings) -> Vec<(Position, String)> {
        let registry = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        placed_labels(&hints_with(text, options, &registry))
    }

    /// The labels of `hints` at their positions.
    fn placed_labels(hints: &[InlayHint]) -> Vec<(Position, String)> {
        hints
            .iter()
            .map(|hint| (hint.position, label_text(&hint.label).to_string()))
            .collect()
    }

    fn labels(hints: &[InlayHint]) -> Vec<String> {
        hints
            .iter()
            .map(|hint| label_text(&hint.label).to_string())
            .collect()
    }

    fn placed(position: Position, label: &str) -> (Position, String) {
        (position, label.to_string())
    }

    #[test]
    fn line_ends_show_each_line_of_an_expression_broken_across_lines() {
        let source = concat!(
            "f = L -> (\n",
            "    if all(L, x -> instance(x, ZZ))\n",
            "    then 1 else 2\n",
            "    )\n",
            "g = L -> if all(L, x -> instance(x, ZZ)) then 1 else 2\n",
            "x = (\n",
            " 2\n",
            " +2\n",
            " )\n",
        );

        // `all(…)` ends line 1 while the `if` goes on, and the `if` ends line
        // 2 inside the parentheses; on line 4 nothing goes on. Of the sum,
        // the line ending in `+2` reads the whole `2 +2`.
        assert_eq!(
            library_hints(source, &showing(&[HintAnchor::LineEnds])),
            [
                placed(pos!(1, 35), "-> Boolean"),
                placed(pos!(2, 17), "ZZ"),
                placed(pos!(7, 3), "ZZ"),
            ]
        );
        // The literal ending line 6 states its type.
        let literal_types = InlayHintSettings {
            literal_types: true,
            ..showing(&[HintAnchor::LineEnds])
        };
        assert!(library_hints(source, &literal_types).contains(&placed(pos!(6, 2), "ZZ")));
    }

    #[test]
    fn cells_show_the_values_macaulay2_prints() {
        let source = concat!(
            "instance(3, ZZ)\n",
            "instance(3, ZZ);\n",
            "x = instance(3, ZZ)\n",
            "isPrime 7\n",
        );

        // Neither a cell ended by `;` nor an assignment, whose binding takes
        // the hint.
        assert_eq!(
            library_hints(source, &showing(&[HintAnchor::Cells])),
            [
                placed(pos!(0, 15), "-> Boolean"),
                placed(pos!(3, 9), "-> Boolean"),
            ]
        );
    }

    #[test]
    fn calls_in_bracketed_form_leave_out_juxtapositions() {
        let source = "isPrime 7\ninstance(3, ZZ)\n";
        let bracketed = showing(&[HintAnchor::Calls]);
        let all = InlayHintSettings {
            call_form: CallForm::All,
            ..bracketed.clone()
        };

        assert_eq!(
            library_hints(source, &bracketed),
            [placed(pos!(1, 15), "-> Boolean")]
        );
        assert_eq!(
            library_hints(source, &all),
            [
                placed(pos!(0, 9), "-> Boolean"),
                placed(pos!(1, 15), "-> Boolean"),
            ]
        );
    }

    #[test]
    fn brackets_closing_arguments_show_what_the_call_returns() {
        assert_eq!(
            library_hints("{instance(3, ZZ)}\n", &showing(&[HintAnchor::Brackets])),
            [
                placed(pos!(0, 16), "-> Boolean"),
                placed(pos!(0, 17), "List"),
            ]
        );
    }

    #[test]
    fn arguments_show_each_argument_of_a_call() {
        // The literal `7` states its type.
        assert_eq!(
            library_hints(
                "instance(isPrime 7, ZZ)\n",
                &showing(&[HintAnchor::Arguments])
            ),
            [
                placed(pos!(0, 18), "-> Boolean"),
                placed(pos!(0, 22), "Ring"),
            ]
        );
    }

    #[test]
    fn excluded_calls_lose_their_hints_and_their_arguments_hints() {
        let options = InlayHintSettings {
            call_form: CallForm::All,
            excluded_calls: names(&["instance"]),
            ..showing(&[HintAnchor::Calls, HintAnchor::Arguments])
        };

        assert_eq!(
            library_hints("instance(isPrime 7, ZZ)\n", &options),
            [placed(pos!(0, 18), "-> Boolean")]
        );
    }

    #[test]
    fn hidden_types_are_never_shown() {
        let options = InlayHintSettings {
            hidden_types: names(&["String"]),
            ..showing(&[HintAnchor::Bindings])
        };

        assert_eq!(
            library_hints("x = isPrime 7\ny = toString 3\n", &options),
            [placed(pos!(0, 1), "Boolean")]
        );
    }

    #[test]
    fn long_labels_are_cut_with_the_whole_type_in_the_tooltip() {
        let registry = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let options = InlayHintSettings {
            max_label_length: Some(4),
            ..showing(&[HintAnchor::Bindings])
        };
        let hints = hints_with("x = isPrime 7\n", &options, &registry);

        assert_eq!(labels(&hints), ["Boo…"]);
        assert!(
            matches!(&hints[0].tooltip, Some(InlayHintTooltip::String(whole)) if whole == "Boolean"),
            "{:?}",
            hints[0].tooltip
        );
    }

    #[test]
    fn types_that_say_nothing_are_shown_only_on_request() {
        let source = "f = x -> x\ny = f 1\n";
        let registry = ObjectRegistry::default();
        let shown = |unknown_types| {
            labels(&hints_with(
                source,
                &InlayHintSettings {
                    unknown_types,
                    ..InlayHintSettings::default()
                },
                &registry,
            ))
        };

        assert!(shown(false).is_empty(), "{:?}", shown(false));
        assert!(shown(true).iter().any(|label| label.contains("Thing")));
    }

    #[test]
    fn calm_default_shows_informative_computed_assignment_types() {
        let computed = hints("x = if condition then 1 else 2\n", false);
        assert_eq!(labels(&computed), vec!["ZZ".to_string()]);
        assert_eq!(computed[0].position, pos!(0, 1));

        let self_describing = concat!(
            "x = 1\n",
            "x = (2)\n",
            "x = [1]\n",
            "f = x -> x\n",
            "y = new MutableList from {1, 2}\n",
            "truth = true\n",
            "falsity = false\n",
            "nothing = null\n",
        );
        let calm = hints(self_describing, false);
        assert_eq!(labels(&calm), ["Thing"]);
        let verbose = hints(self_describing, true);
        assert_eq!(labels(&verbose), ["Thing", "Thing"]);
    }

    #[test]
    fn subexpressions_add_hints_inside_expressions() {
        let source = "x = if condition then 1 else 2\n";
        let calm = hints(source, false);
        let verbose = hints(source, true);
        assert!(
            verbose.len() > calm.len(),
            "subexpressions should add hints: calm={}, verbose={}",
            calm.len(),
            verbose.len()
        );
    }
}
