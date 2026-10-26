//! Type inlay hints derived from static document analysis.

use std::collections::HashMap;

use serde::Deserialize;

use m2_syn::nodes::{ExprLambda as LambdaExpression, ExprNew as NewStatement, Symbol};
use m2_syn::Token;
use tower_lsp::lsp_types::{
    InlayHint, InlayHintKind, InlayHintLabel, InlayHintServerCapabilities, OneOf, Position,
    Range as TextRange,
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

/// Which call results get a type hint when expression types are off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum CallHintPlacement {
    /// None.
    Off,
    /// Those whose call ends its line while the expression around it goes
    /// on below, as the links of a chain broken across lines.
    #[default]
    LineEnds,
    /// Every call.
    Every,
}

pub fn inlay_hint_provider_capability() -> Option<OneOf<bool, InlayHintServerCapabilities>> {
    Some(OneOf::Left(true))
}

pub fn inlay_hints_response(
    document: &DocumentSnapshot,
    range: TextRange,
    options: InlayHintSettings,
    knowledge: &ObjectRegistry,
) -> Vec<InlayHint> {
    let context = HintContext {
        document,
        knowledge,
        options,
        range,
        inferred_types: inferred_expression_types(document, knowledge),
    };
    let binding_hints = context.binding_type_hints();
    let mut hints = binding_hints.clone();
    hints.extend(context.lambda_return_type_hints());

    if options.expression_types || options.all_known_types {
        hints.extend(context.expression_type_hints(&binding_hints, ExpressionSelection::All));
    } else if options.calls != CallHintPlacement::Off {
        hints.extend(
            context
                .expression_type_hints(&binding_hints, ExpressionSelection::Calls(options.calls)),
        );
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

/// The expressions whose types [`HintContext::expression_type_hints`]
/// shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpressionSelection {
    /// Every argument and subexpression.
    All,
    /// Only call results, placed as the setting says.
    Calls(CallHintPlacement),
}

/// What the hints of one request are computed from: the document, the types
/// analysis inferred for its expressions, and the settings choosing which
/// are shown.
struct HintContext<'request, 'tree> {
    document: &'tree DocumentSnapshot,
    knowledge: &'request ObjectRegistry,
    options: InlayHintSettings,
    /// The part of the document the client asks hints for.
    range: TextRange,
    inferred_types: HashMap<SyntaxNodeId, (SyntaxNode<'tree>, InferredType)>,
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

impl<'tree> HintContext<'_, 'tree> {
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
            .filter(|value| self.options.all_known_types || !is_self_describing_value(*value))
            .filter_map(|value| {
                let value_range = self.document.range_for_node(value);
                if !self.range.contains_position(value_range.end) {
                    return None;
                }
                let type_name = self.inferred_type_label(value)?;
                Some(type_hint(
                    value_range.end,
                    &expression_label(value, type_name),
                ))
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
        if inferred_type.is_unknown() && !self.options.unknown_types {
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

    /// The types of bindings, after their names: where a binding is first
    /// assigned or its type changes, unless the value states it.
    fn binding_type_hints(&self) -> Vec<InlayHint> {
        let all_known_types = self.options.all_known_types;
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
                if !all_known_types && !changed && assigned.is_none_or(|value| !value.destructured)
                {
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
                if self.range.contains_position(position) {
                    hints.push(type_hint(position, &type_name));
                }
            }
        }

        hints
    }

    /// The types of the expressions `selection` picks, after them; a call
    /// result reads `-> T`. None repeats a binding hint in `binding_hints`.
    fn expression_type_hints(
        &self,
        binding_hints: &[InlayHint],
        selection: ExpressionSelection,
    ) -> Vec<InlayHint> {
        let all_known_types = self.options.all_known_types;
        let binding_value_types: Vec<TypeHintIdentity> = binding_hints
            .iter()
            .filter(|hint| hint.kind == Some(InlayHintKind::TYPE))
            .map(|hint| TypeHintIdentity {
                position: hint.position,
                label: label_text(&hint.label).to_string(),
            })
            .collect();
        let opaque_self_describing_values = self
            .self_describing_assignment_value_nodes()
            .into_iter()
            .filter(|node| !parenthesized_value(*node).is::<LambdaExpression>())
            .map(|node| self.document.range_for_node(node))
            .collect::<Vec<_>>();
        let assignment_parts = self.assignment_parts();
        self.inferred_types
            .values()
            .filter_map(|(node, inferred_type)| {
                let node = *node;
                let expression_range = self.document.range_for_node(node);
                let is_call = node.is_space_application();
                let selected = match selection {
                    ExpressionSelection::All => true,
                    ExpressionSelection::Calls(CallHintPlacement::Every) => is_call,
                    ExpressionSelection::Calls(_) => {
                        is_call && self.ends_line_of_longer_expression(node)
                    }
                };
                if !selected
                    || !range_contains(self.range, expression_range)
                    || self.type_is_stated_by_installation(node)
                    || (!all_known_types
                        && (assignment_parts.suppresses(expression_range, is_call)
                            || is_self_describing_value(node)
                            || opaque_self_describing_values
                                .iter()
                                .any(|value_range| range_contains(*value_range, expression_range))))
                {
                    return None;
                }
                let type_name = self.displayed_type(expression_range.start, inferred_type)?;
                if binding_value_types.contains(&TypeHintIdentity {
                    position: expression_range.end,
                    label: type_name.clone(),
                }) {
                    return None;
                }
                Some(type_hint(
                    expression_range.end,
                    &expression_label(node, type_name),
                ))
            })
            .collect()
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
    use super::*;
    use crate::object_registry::ObjectRegistry;

    fn hints(text: &str, expression_types: bool) -> Vec<InlayHint> {
        hints_with(
            text,
            InlayHintSettings {
                expression_types,
                unknown_types: true,
                ..InlayHintSettings::default()
            },
            &ObjectRegistry::default(),
        )
    }

    fn hints_with(
        text: &str,
        options: InlayHintSettings,
        registry: &ObjectRegistry,
    ) -> Vec<InlayHint> {
        let document =
            DocumentSnapshot::from_text(text.to_string(), registry).expect("fixture should parse");
        let range = TextRange::new(pos!(), pos!(u32::MAX, 0));
        inlay_hints_response(&document, range, options, registry)
    }

    /// The labels of `hints` at their positions.
    fn placed_labels(hints: &[InlayHint]) -> Vec<(Position, String)> {
        hints
            .iter()
            .map(|hint| (hint.position, label_text(&hint.label).to_string()))
            .collect()
    }

    #[test]
    fn call_results_show_where_a_longer_expression_breaks_its_line() {
        let registry = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let source = concat!(
            "f = L -> (
",
            "    if all(L, x -> instance(x, ZZ))
",
            "    then 1 else 2
",
            "    )
",
            "g = L -> if all(L, x -> instance(x, ZZ)) then 1 else 2
",
        );
        let call_hints = |calls| {
            placed_labels(&hints_with(
                source,
                InlayHintSettings {
                    calls,
                    ..InlayHintSettings::default()
                },
                &registry,
            ))
            .into_iter()
            .filter(|(_, label)| label == "-> Boolean")
            .map(|(position, _)| position)
            .collect::<Vec<_>>()
        };

        // The lambdas return what `instance` does, after its call.
        let lambda_returns = vec![pos!(1, 34), pos!(4, 39)];
        assert_eq!(call_hints(CallHintPlacement::Off), lambda_returns);
        // `all(…)` ends line 1 while the `if` goes on; on line 4 it does not.
        assert_eq!(
            call_hints(CallHintPlacement::LineEnds),
            vec![pos!(1, 34), pos!(1, 35), pos!(4, 39)]
        );
        assert_eq!(
            call_hints(CallHintPlacement::Every),
            vec![pos!(1, 34), pos!(1, 35), pos!(4, 39), pos!(4, 40)]
        );
    }

    #[test]
    fn types_that_say_nothing_are_shown_only_on_request() {
        let source = "f = x -> x
y = f 1
";
        let registry = ObjectRegistry::default();
        let shown = |unknown_types| {
            labels(&hints_with(
                source,
                InlayHintSettings {
                    unknown_types,
                    ..InlayHintSettings::default()
                },
                &registry,
            ))
        };

        assert!(shown(false).is_empty(), "{:?}", shown(false));
        assert!(shown(true).iter().any(|label| label.contains("Thing")));
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
