//! Type inlay hints derived from static document analysis.

use std::collections::HashMap;

use m2_syn::nodes::{ExprLambda as LambdaExpression, ExprNew as NewStatement, Symbol};
use m2_syn::Token;
use tower_lsp::lsp_types::{
    InlayHint, InlayHintKind, InlayHintLabel, InlayHintServerCapabilities, OneOf, Position,
    Range as TextRange,
};

use crate::document::DocumentSnapshot;
use crate::node_metadata::{M2Node, SyntaxNodeId};
use crate::object_registry::ObjectRegistry;
use crate::source::SourceNavigation;
use crate::typesystem::InferredType;
use crate::util::TextRangeExt;

pub fn inlay_hint_provider_capability() -> Option<OneOf<bool, InlayHintServerCapabilities>> {
    Some(OneOf::Left(true))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InlayHintOptions {
    pub expression_types: bool,
    pub all_known_types: bool,
}

pub fn inlay_hints_response(
    document: &DocumentSnapshot,
    range: TextRange,
    options: InlayHintOptions,
    knowledge: &ObjectRegistry,
) -> Vec<InlayHint> {
    let inferred_types = inferred_expression_types(document, knowledge);
    let binding_hints = binding_type_hints(
        document,
        &range,
        options.all_known_types,
        knowledge,
        &inferred_types,
    );
    let mut hints = binding_hints.clone();
    hints.extend(lambda_return_type_hints(
        document,
        &range,
        options.all_known_types,
        knowledge,
        &inferred_types,
    ));

    if options.expression_types || options.all_known_types {
        hints.extend(expression_type_hints(
            document,
            &range,
            options.all_known_types,
            knowledge,
            &inferred_types,
            &binding_hints,
        ));
    }
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

fn hint_order(label: &InlayHintLabel) -> bool {
    label_text(label).starts_with("-> ")
}

fn inferred_expression_types<'tree>(
    document: &'tree DocumentSnapshot,
    knowledge: &ObjectRegistry,
) -> HashMap<SyntaxNodeId, (M2Node<'tree>, InferredType)> {
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

fn lambda_return_type_hints(
    document: &DocumentSnapshot,
    range: &TextRange,
    all_known_types: bool,
    knowledge: &ObjectRegistry,
    inferred_types: &HashMap<SyntaxNodeId, (M2Node<'_>, InferredType)>,
) -> Vec<InlayHint> {
    let analysis = document.analysis();
    inferred_types
        .values()
        .filter_map(|(node, _)| {
            let node = *node;
            if node.is_return_expr() {
                let value = node.control_transfer_value()?;
                let view = knowledge.at(document.position_for_node(value));
                analysis.control_transfer_target(node, document, &view)?;
                Some(value)
            } else if node.is::<LambdaExpression>() {
                lambda_final_value(node)
            } else {
                None
            }
        })
        .filter(|value| all_known_types || !is_self_describing_value(*value))
        .filter_map(|value| {
            let value_range = document.range_for_node(value);
            if !range.contains_position(value_range.end) {
                return None;
            }
            let type_name = inferred_type_label(document, knowledge, inferred_types, value)?;
            Some(type_hint(value_range.end, &type_name))
        })
        .collect()
}

fn lambda_final_value(lambda: M2Node<'_>) -> Option<M2Node<'_>> {
    let mut value = lambda.child_by_field_name("body")?;
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

fn inferred_type_label(
    document: &DocumentSnapshot,
    knowledge: &ObjectRegistry,
    inferred_types: &HashMap<SyntaxNodeId, (M2Node<'_>, InferredType)>,
    node: M2Node<'_>,
) -> Option<String> {
    displayed_type(
        document,
        document.position_for_node(node),
        knowledge,
        &inferred_types.get(&node.id())?.1,
    )
}

fn displayed_type(
    document: &DocumentSnapshot,
    position: Position,
    knowledge: &ObjectRegistry,
    inferred_type: &InferredType,
) -> Option<String> {
    let view = knowledge.at(position);
    inferred_type
        .subset_label(|generator| {
            document
                .analysis()
                .has_strict_subtype_at(generator, position, &view)
        })
        .map(inlay_type_label)
}

fn type_hint(position: Position, type_name: &str) -> InlayHint {
    InlayHint {
        position,
        label: InlayHintLabel::from(type_name.to_string()),
        kind: Some(InlayHintKind::TYPE),
        text_edits: None,
        tooltip: None,
        padding_left: Some(true),
        padding_right: None,
        data: None,
    }
}

struct AssignedValue {
    target_range: TextRange,
    value_range: TextRange,
    type_name: String,
    destructured: bool,
}

#[derive(PartialEq, Eq)]
struct TypeHintIdentity {
    position: Position,
    label: String,
}

fn binding_type_hints(
    document: &DocumentSnapshot,
    range: &TextRange,
    all_known_types: bool,
    knowledge: &ObjectRegistry,
    inferred_types: &HashMap<SyntaxNodeId, (M2Node<'_>, InferredType)>,
) -> Vec<InlayHint> {
    let assigned_values = assigned_values(document, knowledge, inferred_types);
    let self_describing_values = self_describing_assignment_value_nodes(inferred_types)
        .into_iter()
        .map(|node| document.range_for_node(node))
        .collect::<Vec<_>>();
    let mut hints = Vec::new();

    for binding in document.analysis().bindings() {
        let mut previous_type: Option<String> = None;
        for state in &binding.states {
            if binding.role == crate::meta::BindingRole::Parameter && state.inferred_type.is_some()
            {
                continue;
            }
            let assigned = assigned_values
                .iter()
                .find(|value| value.target_range == state.span);
            let type_name = assigned.map(|value| value.type_name.clone()).or_else(|| {
                state.inferred_type.as_ref().and_then(|inferred_type| {
                    displayed_type(document, state.span.start, knowledge, inferred_type)
                })
            });
            let Some(type_name) = type_name else {
                previous_type = None;
                continue;
            };
            let changed = previous_type.as_ref() != Some(&type_name);
            previous_type = Some(type_name.clone());
            if !all_known_types && !changed && assigned.is_none_or(|value| !value.destructured) {
                continue;
            }
            let value_range = assigned
                .map(|value| value.value_range)
                .or(state.value_range);
            if !all_known_types
                && value_range.is_some_and(|value_range| {
                    self_describing_values.contains(&value_range)
                        && assigned.is_none_or(|value| !value.destructured)
                })
            {
                continue;
            }
            let position = state.span.end;
            if range.contains_position(position) {
                hints.push(type_hint(position, &type_name));
            }
        }
    }

    hints
}

fn expression_type_hints(
    document: &DocumentSnapshot,
    range: &TextRange,
    all_known_types: bool,
    knowledge: &ObjectRegistry,
    inferred_types: &HashMap<SyntaxNodeId, (M2Node<'_>, InferredType)>,
    binding_hints: &[InlayHint],
) -> Vec<InlayHint> {
    let binding_value_types: Vec<TypeHintIdentity> = binding_hints
        .iter()
        .filter(|hint| hint.kind == Some(InlayHintKind::TYPE))
        .map(|hint| TypeHintIdentity {
            position: hint.position,
            label: label_text(&hint.label).to_string(),
        })
        .collect();
    let opaque_self_describing_values = self_describing_assignment_value_nodes(inferred_types)
        .into_iter()
        .filter(|node| !parenthesized_value(*node).is::<LambdaExpression>())
        .map(|node| document.range_for_node(node))
        .collect::<Vec<_>>();
    let assignment_parts = assignment_parts(document, inferred_types);
    inferred_types
        .values()
        .filter_map(|(node, inferred_type)| {
            let expression_range = document.range_for_node(*node);
            let is_call = node.is_space_application();
            if !range_contains(*range, expression_range)
                || type_is_stated_by_installation(document, *node)
                || (!all_known_types
                    && (assignment_parts.suppresses(expression_range, is_call)
                        || is_self_describing_value(*node)
                        || opaque_self_describing_values
                            .iter()
                            .any(|value_range| range_contains(*value_range, expression_range))))
            {
                return None;
            }
            let type_name =
                displayed_type(document, expression_range.start, knowledge, inferred_type)?;
            let end = expression_range.end;
            if binding_value_types.contains(&TypeHintIdentity {
                position: end,
                label: type_name.clone(),
            }) {
                return None;
            }
            let label = if is_call {
                format!("-> {type_name}")
            } else {
                type_name
            };
            Some(type_hint(expression_range.end, &label))
        })
        .collect()
}

fn type_is_stated_by_installation(document: &DocumentSnapshot, node: M2Node<'_>) -> bool {
    node.is::<Symbol>()
        && document
            .analysis()
            .get_binding_at(node.text(), document.position_for_node(node))
            .is_some_and(|binding| {
                binding.role == crate::meta::BindingRole::Parameter
                    && binding.state.inferred_type.is_some()
            })
}

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

fn assignment_parts(
    document: &DocumentSnapshot,
    inferred_types: &HashMap<SyntaxNodeId, (M2Node<'_>, InferredType)>,
) -> AssignmentParts {
    let mut parts = AssignmentParts {
        assignments: Vec::new(),
        targets: Vec::new(),
        values: Vec::new(),
    };
    for assignment in inferred_types
        .values()
        .map(|(node, _)| *node)
        .filter(M2Node::is_assignment)
    {
        parts.assignments.push(document.range_for_node(assignment));
        if let Some(target) = assignment.child_by_field_name("left") {
            parts.targets.push(document.range_for_node(target));
        }
        if let Some(value) = assignment.child_by_field_name("right") {
            parts.values.push(document.range_for_node(value));
        }
    }
    parts
}

fn assigned_values(
    document: &DocumentSnapshot,
    knowledge: &ObjectRegistry,
    inferred_types: &HashMap<SyntaxNodeId, (M2Node<'_>, InferredType)>,
) -> Vec<AssignedValue> {
    let mut values = Vec::new();
    for assignment in inferred_types
        .values()
        .map(|(node, _)| *node)
        .filter(|node| {
            node.has_binary_operator::<Token![=]>() || node.has_binary_operator::<Token![:=]>()
        })
    {
        let (Some(left), Some(right)) = (
            assignment.child_by_field_name("left"),
            assignment.child_by_field_name("right"),
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
            let value_range = document.range_for_node(value);
            let Some(type_name) = inferred_type_label(document, knowledge, inferred_types, value)
            else {
                continue;
            };
            values.push(AssignedValue {
                target_range: document.range_for_node(target),
                value_range,
                type_name,
                destructured,
            });
        }
    }
    values
}

fn paired_assignment_values<'tree>(
    target: M2Node<'tree>,
    value: M2Node<'tree>,
) -> Vec<(M2Node<'tree>, M2Node<'tree>)> {
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

fn self_describing_assignment_value_nodes<'tree>(
    inferred_types: &HashMap<SyntaxNodeId, (M2Node<'tree>, InferredType)>,
) -> Vec<M2Node<'tree>> {
    inferred_types
        .values()
        .map(|(node, _)| *node)
        .filter(|node| node.is_assignment())
        .filter_map(|assignment| {
            let left = assignment.child_by_field_name("left")?;
            let right = assignment.child_by_field_name("right")?;
            (left.is::<Symbol>() && is_self_describing_value(right)).then_some(right)
        })
        .collect()
}

fn is_self_describing_value(node: M2Node<'_>) -> bool {
    let node = parenthesized_value(node);
    node.is_literal()
        || node.is_nothing_value()
        || (node.is::<Symbol>() && matches!(node.text(), "true" | "false" | "null"))
        || node.is::<LambdaExpression>()
        || node.is::<NewStatement>()
        || ((node.is_collection_expression() || node.is_sequence())
            && node.collection_elements().all(is_self_describing_value))
}

fn parenthesized_value(mut node: M2Node<'_>) -> M2Node<'_> {
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
    use super::*;
    use crate::object_registry::ObjectRegistry;

    fn hints(text: &str, expression_types: bool) -> Vec<InlayHint> {
        let registry = ObjectRegistry::default();
        let document =
            DocumentSnapshot::from_text(text.to_string(), &registry).expect("fixture should parse");
        let range = TextRange::new(pos!(), pos!(u32::MAX, 0));
        inlay_hints_response(
            &document,
            range,
            InlayHintOptions {
                expression_types,
                all_known_types: false,
            },
            &registry,
        )
    }

    fn labels(hints: &[InlayHint]) -> Vec<String> {
        hints
            .iter()
            .map(|hint| label_text(&hint.label).to_string())
            .collect()
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
    fn expression_types_opt_in_adds_sub_expression_hints() {
        // The maximal readout is opt-in and yields strictly more hints than the
        // calm default (it annotates sub-expressions too).
        let source = "x = if condition then 1 else 2\n";
        let calm = hints(source, false);
        let verbose = hints(source, true);
        assert!(
            verbose.len() > calm.len(),
            "opt-in readout should add hints: calm={}, verbose={}",
            calm.len(),
            verbose.len()
        );
    }

    #[test]
    fn calm_default_does_not_annotate_bare_expressions() {
        // A bare expression statement (not a binding) gets no hint in calm mode.
        let hints = hints("f 1\n", false);
        assert!(hints.is_empty(), "got {:?}", labels(&hints));
    }
}
