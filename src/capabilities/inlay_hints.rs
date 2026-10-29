//! Type inlay hints derived from static document analysis.
//!
//! Each kind of place the settings turn on picks the values it annotates,
//! and one filter shared by all of them drops the hints the settings leave
//! out, such as those repeating what the source or a nearby hint already
//! says. Of the places giving the type of the same value, such as an
//! assignment, the cell it ends and the line it ends, only one is shown.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::iter;

use m2_syn::nodes::{ExprLambda as LambdaExpression, ExprNew as NewStatement, Symbol};
use m2_syn::Token;
use tower_lsp::lsp_types::{
    InlayHint, InlayHintKind, InlayHintLabel, InlayHintServerCapabilities, InlayHintTooltip, OneOf,
    Position, Range as TextRange,
};

use crate::document::DocumentSnapshot;
use crate::meta::BindingRole;
use crate::node_metadata::{SyntaxNode, SyntaxNodeId};
use crate::object_registry::{ObjectName, ObjectRegistry};
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

/// The kind of place a hint is put at. Of the hints giving the type of the
/// same value, the one of the earliest kind is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HintSource {
    /// After the name an assignment binds.
    Assignment,
    /// After the value a function returns.
    Return,
    /// After a parenthesized value.
    Parenthesized,
    /// After a call with bracketed arguments.
    Call,
    /// After a top-level expression.
    Cell,
    /// At the end of a line.
    LineEnd,
    /// After any expression.
    Everywhere,
    /// Before a call's argument, of the parameter it is passed to.
    Argument,
}

/// A hint, with the value it gives the type of, if any, and the kind of
/// place that put it there.
struct PlacedHint {
    /// The range of the value, as [`described_value`] finds it.
    value: Option<TextRange>,
    /// Semantic origin used to order and deduplicate hints at the same position.
    source: HintSource,
    /// Protocol inlay hint emitted by this source.
    hint: InlayHint,
}

impl PlacedHint {
    /// The hints of `placed`, one for each value: the one of the earliest
    /// source, so the same type is not shown twice.
    fn one_per_value(placed: Vec<Self>) -> Vec<InlayHint> {
        let mut kept = BTreeMap::<(Position, Position), Self>::new();
        let mut hints = Vec::new();
        for hint in placed {
            let Some(value) = hint.value else {
                hints.push(hint.hint);
                continue;
            };
            match kept.entry((value.start, value.end)) {
                Entry::Vacant(entry) => {
                    entry.insert(hint);
                }
                Entry::Occupied(mut entry) => {
                    if hint.precedes(entry.get()) {
                        entry.insert(hint);
                    }
                }
            }
        }
        hints.extend(kept.into_values().map(|placed| placed.hint));
        hints
    }

    /// Whether `self` is kept over `other`, a hint of the same value.
    fn precedes(&self, other: &Self) -> bool {
        (self.source, label_text(&self.hint.label)) < (other.source, label_text(&other.hint.label))
    }
}

/// Where the hint of a value sits, and how it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HintPlacement {
    /// Right after the value: its type, or `-> T` for a call's result.
    AfterValue,
    /// At this position, the end of the line the value ends on.
    LineEnd(Position),
}

/// What a hinted type is the type of, which decides how it is labeled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypedSubject {
    /// A value, whose type stands for its subtypes too only when the type
    /// says so, as `↑Ring` does.
    Value,
    /// The domain a method is installed for, which always takes the
    /// subtypes too, so its label never says so.
    ParameterDomain,
}

/// What is known of the parameter a call passes one of its arguments to.
struct CalledParameter {
    /// The name the called lambda binds it to.
    name: Option<String>,
    /// The type the called method is installed for there.
    domain: Option<ObjectName>,
}

/// Advertises inlay-hint support in the server initialization response.
pub fn inlay_hint_provider_capability() -> Option<OneOf<bool, InlayHintServerCapabilities>> {
    Some(OneOf::Left(true))
}

/// Collects configured hints for the requested range and removes duplicate type presentations.
pub fn inlay_hints_response(
    document: &DocumentSnapshot,
    range: TextRange,
    options: &InlayHintSettings,
    knowledge: &ObjectRegistry,
) -> Vec<InlayHint> {
    let mut hints = PlacedHint::one_per_value(
        HintContext::new(document, knowledge, options, range).placed_hints(),
    );
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
    /// Immutable document snapshot being queried.
    document: &'tree DocumentSnapshot,
    /// Positioned object and type facts available to this request.
    knowledge: &'request ObjectRegistry,
    /// Options accepted by the callable, or settings governing this request.
    options: &'request InlayHintSettings,
    /// The part of the document the client asks hints for.
    range: TextRange,
    /// Expression identities paired with their syntax and inferred types.
    inferred_types: HashMap<SyntaxNodeId, (SyntaxNode<'tree>, InferredType)>,
    /// The parts of the document's assignments, whose types binding hints
    /// show.
    assignment_parts: AssignmentParts,
    /// The values assigned to names that state their own type, other than
    /// functions: nothing in them gets a hint.
    opaque_self_describing_values: Vec<TextRange>,
}

/// Ordering key used to place related hints consistently at one position.
fn hint_order(label: &InlayHintLabel) -> bool {
    label_text(label).starts_with(CALL_RESULT_PREFIX)
}

/// Expression types derived from the available type evidence.
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
    /// Constructs this value from the supplied dependencies and initial state.
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

    /// The hints of every kind of place the settings turn on.
    fn placed_hints(&self) -> Vec<PlacedHint> {
        let options = self.options;
        let mut hints = Vec::new();
        if options.during_assignment {
            hints.extend(self.binding_type_hints());
        }
        if options.after_return {
            hints.extend(self.lambda_return_type_hints());
        }
        if options.after_cell {
            hints.extend(self.value_hints(
                self.cell_values(),
                HintSource::Cell,
                HintPlacement::AfterValue,
            ));
        }
        if options.after_call {
            hints.extend(self.value_hints(
                self.typed_nodes().filter(|node| node.is_bracketed_call()),
                HintSource::Call,
                HintPlacement::AfterValue,
            ));
        }
        if options.after_brackets {
            hints.extend(
                self.value_hints(
                    self.typed_nodes()
                        .filter(|node| node.is_holder() && applied_call(*node).is_none()),
                    HintSource::Parenthesized,
                    HintPlacement::AfterValue,
                ),
            );
        }
        if options.after_line_end {
            hints.extend(self.line_end_hints());
        }
        if options.arg_names || options.arg_types {
            hints.extend(self.parameter_hints().into_iter().map(|hint| PlacedHint {
                value: None,
                source: HintSource::Argument,
                hint,
            }));
        }
        if options.everywhere {
            hints.extend(self.value_hints(
                self.typed_nodes(),
                HintSource::Everywhere,
                HintPlacement::AfterValue,
            ));
        }
        hints
    }

    /// The expressions analysis inferred a type for.
    fn typed_nodes(&self) -> impl Iterator<Item = SyntaxNode<'tree>> + '_ {
        self.inferred_types.values().map(|(node, _)| *node)
    }

    /// The hints of each of `values` placed at `placement` that the filters
    /// keep.
    fn value_hints(
        &self,
        values: impl IntoIterator<Item = SyntaxNode<'tree>>,
        source: HintSource,
        placement: HintPlacement,
    ) -> Vec<PlacedHint> {
        values
            .into_iter()
            .filter_map(|value| self.value_hint(value, source, placement))
            .collect()
    }

    /// The hint of `value` at `placement`, of its type, unless the settings
    /// leave it out.
    fn value_hint(
        &self,
        value: SyntaxNode<'tree>,
        source: HintSource,
        placement: HintPlacement,
    ) -> Option<PlacedHint> {
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
            || (self.options.hide_after_semicolon && is_discarded_by_semicolon(value))
            || self.is_excluded_call(value)
            || self.type_is_stated_by_installation(value)
            || (!self.options.show_repeated && repeated())
            || (!self.options.show_literal && stated_by_literal())
        {
            return None;
        }
        let type_name =
            self.displayed_type(value_range.start, inferred_type, TypedSubject::Value)?;
        let position = match placement {
            HintPlacement::AfterValue => value_range.end,
            HintPlacement::LineEnd(line_end) => line_end,
        };
        Some(PlacedHint {
            value: Some(self.document.range_for_node(described_value(value))),
            source,
            hint: self.type_hint(position, expression_label(value, type_name)),
        })
    }

    /// The parameters each call passes its arguments to, before the
    /// arguments, as `f(x: Number 2)`; none for a call whose parameters do
    /// not match its arguments one to one.
    fn parameter_hints(&self) -> Vec<InlayHint> {
        let lambdas = self
            .document
            .root_node()
            .descendants()
            .filter(|node| node.is::<LambdaExpression>())
            .collect::<Vec<_>>();
        self.typed_nodes()
            .filter(|call| call.is_space_application() && !self.is_excluded_call(*call))
            .flat_map(|call| {
                let arguments = call_arguments(call);
                let parameters = self.called_parameters(call, &lambdas);
                if arguments.len() != parameters.len() {
                    return Vec::new();
                }
                iter::zip(arguments, parameters)
                    .filter_map(|(argument, parameter)| self.parameter_hint(argument, parameter))
                    .collect()
            })
            .collect()
    }

    /// The parameters `call` passes its arguments to, as far as they are
    /// known: of the one local method installation or library signature the
    /// argument types select, or of the lambda a local function is.
    fn called_parameters(
        &self,
        call: SyntaxNode<'tree>,
        lambdas: &[SyntaxNode<'tree>],
    ) -> Vec<CalledParameter> {
        let Some(parts) = call.binary_parts() else {
            return Vec::new();
        };
        let (head, argument) = (parts.left, parts.right);
        if !head.is::<Symbol>() {
            return Vec::new();
        }
        let analysis = self.document.analysis();
        let position = self.document.position_for_node(head);
        let view = self.knowledge.at(position);
        let names = |lambda: Option<&SyntaxNode<'tree>>| {
            lambda.map(|lambda| {
                lambda
                    .lambda_parameter_symbols()
                    .iter()
                    .map(|symbol| symbol.text().to_string())
                    .collect::<Vec<_>>()
            })
        };
        let Some(function) = analysis.function_at(head.text(), position) else {
            let facts = analysis.infer_call_static_facts(argument, self.document, &view);
            let Some(signature) = analysis
                .infer_external_call_signature_facts(
                    &ObjectName::new(head.text()),
                    &facts,
                    position,
                    &view,
                )
                .and_then(|facts| facts.pinned)
            else {
                return Vec::new();
            };
            return signature
                .domain
                .iter()
                .map(|object| CalledParameter {
                    name: None,
                    domain: self
                        .knowledge
                        .object(object)
                        .map(|record| record.name.clone()),
                })
                .collect();
        };
        if !function.is_method_function() {
            let value_range = analysis
                .get_binding_at(head.text(), position)
                .and_then(|binding| binding.state.value_range);
            let lambda = lambdas
                .iter()
                .find(|lambda| value_range == Some(self.document.range_for_node(**lambda)));
            return names(lambda)
                .unwrap_or_default()
                .into_iter()
                .map(|name| CalledParameter {
                    name: Some(name),
                    domain: None,
                })
                .collect();
        }
        let installations =
            analysis.local_call_installations(function, argument, position, self.document, &view);
        let [installation] = installations.as_slice() else {
            return Vec::new();
        };
        let lambda = lambdas.iter().find(|lambda| {
            analysis
                .installation_receiving_arguments(**lambda, self.document)
                .is_some_and(|receiving| receiving.id == installation.id)
        });
        let names = names(lambda).filter(|names| names.len() == installation.method.domain.len());
        installation
            .method
            .domain
            .iter()
            .enumerate()
            .map(|(index, domain)| CalledParameter {
                name: names.as_ref().map(|names| names[index].clone()),
                domain: Some(domain.clone()),
            })
            .collect()
    }

    /// The hint before `argument` of the `parameter` it is passed to, of
    /// what the settings show of it: `x: T`, `x:` or `T`. A name the
    /// argument already spells is left out.
    fn parameter_hint(
        &self,
        argument: SyntaxNode<'_>,
        parameter: CalledParameter,
    ) -> Option<InlayHint> {
        let position = self.document.position_for_node(argument);
        if !self.range.contains_position(position) {
            return None;
        }
        let name = parameter
            .name
            .filter(|name| self.options.arg_names && name != argument.text());
        let type_name = parameter
            .domain
            .filter(|_| self.options.arg_types)
            .and_then(|domain| {
                self.displayed_type(
                    position,
                    &InferredType::upward_from_id(domain),
                    TypedSubject::ParameterDomain,
                )
            });
        let label = match (name, type_name) {
            (Some(name), Some(type_name)) => format!("{name}: {type_name}"),
            (Some(name), None) => format!("{name}:"),
            (None, Some(type_name)) => type_name,
            (None, None) => return None,
        };
        Some(InlayHint {
            kind: Some(InlayHintKind::PARAMETER),
            padding_left: None,
            padding_right: Some(true),
            ..self.type_hint(position, label)
        })
    }

    /// The hints at the ends of lines, each of the last expression complete
    /// on its line: the outermost of those ending last there, unless an
    /// expression starting after it on the line goes on below, as one an
    /// opening bracket starts.
    fn line_end_hints(&self) -> Vec<PlacedHint> {
        let line_of = |byte| self.document.position_for_byte(byte).line;
        let mut last_complete = HashMap::<u32, SyntaxNode<'tree>>::new();
        let mut last_continued_start = HashMap::<u32, usize>::new();
        for node in self.typed_nodes().filter(|node| !node.is_muted_statement()) {
            let (start_line, end_line) = (line_of(node.start_byte()), line_of(node.end_byte()));
            if start_line < end_line {
                let start = last_continued_start.entry(start_line).or_default();
                *start = (*start).max(node.start_byte());
            }
            let ends_later = |other: SyntaxNode<'_>| {
                (node.end_byte(), node.end_byte() - node.start_byte())
                    > (other.end_byte(), other.end_byte() - other.start_byte())
            };
            last_complete
                .entry(end_line)
                .and_modify(|last| {
                    if ends_later(*last) {
                        *last = node;
                    }
                })
                .or_insert(node);
        }
        let content_ends = self.line_content_ends();
        last_complete
            .into_iter()
            .filter(|(line, value)| {
                last_continued_start
                    .get(line)
                    .is_none_or(|start| *start < value.end_byte())
            })
            .filter_map(|(line, value)| {
                let line_end = content_ends.get(&line).copied().unwrap_or(value.end_byte());
                self.value_hint(
                    value,
                    HintSource::LineEnd,
                    HintPlacement::LineEnd(self.document.position_for_byte(line_end)),
                )
            })
            .collect()
    }

    /// Where the code of each line ends, before any comment closing it: the
    /// end of the last token on it.
    fn line_content_ends(&self) -> HashMap<u32, usize> {
        let mut ends = HashMap::<u32, usize>::new();
        for token in self
            .document
            .root_node()
            .descendants()
            .filter(|node| node.children().next().is_none() && !node.is_comment())
        {
            let end = ends
                .entry(self.document.position_for_byte(token.end_byte()).line)
                .or_default();
            *end = (*end).max(token.end_byte());
        }
        ends
    }

    /// The values of the top-level expressions, each as Macaulay2's
    /// `oN : T` line would read.
    fn cell_values(&self) -> impl Iterator<Item = SyntaxNode<'tree>> + '_ {
        self.document
            .root_node()
            .children()
            .filter_map(|cell| cell.final_value_child())
    }

    /// Whether `node` calls a function the settings exclude.
    fn is_excluded_call(&self, node: SyntaxNode<'_>) -> bool {
        node.applied_symbol_name()
            .is_some_and(|name| self.options.excluded_calls.contains(name))
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
    fn lambda_return_type_hints(&self) -> Vec<PlacedHint> {
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
            .filter(|value| self.options.show_literal || !is_self_describing_value(*value))
            .filter_map(|value| {
                let value_range = self.document.range_for_node(value);
                if !self.range.contains_position(value_range.end) {
                    return None;
                }
                let type_name = self.inferred_type_label(value)?;
                Some(PlacedHint {
                    value: Some(self.document.range_for_node(described_value(value))),
                    source: HintSource::Return,
                    hint: self.type_hint(value_range.end, expression_label(value, type_name)),
                })
            })
            .collect()
    }

    /// The label of the inferred type of `node`, when it is shown.
    fn inferred_type_label(&self, node: SyntaxNode<'_>) -> Option<String> {
        self.displayed_type(
            self.document.position_for_node(node),
            &self.inferred_types.get(&node.id())?.1,
            TypedSubject::Value,
        )
    }

    /// The label of `inferred_type`, the type of `subject` at `position`;
    /// none for a type that says nothing, or only that the value is `null`,
    /// unless the settings ask for those.
    fn displayed_type(
        &self,
        position: Position,
        inferred_type: &InferredType,
        subject: TypedSubject,
    ) -> Option<String> {
        let is_nothing = inferred_type
            .single()
            .is_some_and(|name| name.name() == NOTHING_TYPE);
        if (inferred_type.is_unknown() && !self.options.show_thing)
            || (is_nothing && !self.options.show_nothing)
            || self.is_hidden(inferred_type)
        {
            return None;
        }
        let view = self.knowledge.at(position);
        inferred_type
            .subset_label(|generator| {
                subject == TypedSubject::Value
                    && self.options.show_upper_closure
                    && self
                        .document
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
        members.peek().is_some() && members.all(|member| self.options.hide_types.contains(member))
    }

    /// The types of bindings, after their names: where a binding is first
    /// assigned or its type changes, unless the value states it.
    fn binding_type_hints(&self) -> Vec<PlacedHint> {
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
                        self.displayed_type(state.span.start, inferred_type, TypedSubject::Value)
                    })
                });
                let Some(type_name) = type_name else {
                    previous_type = None;
                    continue;
                };
                let changed = previous_type.as_ref() != Some(&type_name);
                previous_type = Some(type_name.clone());
                if !self.options.show_repeated
                    && !changed
                    && assigned.is_none_or(|value| !value.destructured)
                {
                    continue;
                }
                let value_range = assigned
                    .map(|value| value.value_range)
                    .or(state.value_range);
                if !self.options.show_literal
                    && value_range.is_some_and(|value_range| {
                        self_describing_values.contains(&value_range)
                            && assigned.is_none_or(|value| !value.destructured)
                    })
                {
                    continue;
                }
                let position = state.span.end;
                if self.range.contains_position(position) {
                    hints.push(PlacedHint {
                        value: assigned.map(|value| value.described_range),
                        source: HintSource::Assignment,
                        hint: self.type_hint(position, type_name),
                    });
                }
            }
        }

        hints
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

    /// Target and value operands extracted from supported assignment syntax.
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

    /// Target/value pairs eligible for assignment type hints.
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
                    described_range: self.document.range_for_node(described_value(value)),
                    type_name,
                    destructured,
                });
            }
        }
        values
    }

    /// Assignment value nodes whose literal syntax already communicates the displayed type.
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

/// Final expression contributing the lambda's return value.
fn lambda_final_value(lambda: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let mut value = lambda.lambda_body()?;
    while value.is_holder() {
        value = value.final_value_child()?;
    }
    (!value.is_return_expr()).then_some(value)
}

/// Plain text of either protocol inlay-label representation.
fn label_text(label: &InlayHintLabel) -> &str {
    match label {
        InlayHintLabel::String(text) => text,
        InlayHintLabel::LabelParts(parts) => {
            parts.first().map(|part| part.value.as_str()).unwrap_or("")
        }
    }
}

/// Formats an inferred type name as an inlay label.
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

/// Whether nothing but a `;` follows `value` in the statement it ends, so
/// its value is discarded.
fn is_discarded_by_semicolon(value: SyntaxNode<'_>) -> bool {
    value
        .ancestors()
        .find(|ancestor| ancestor.is_muted_statement() || ancestor.end_byte() != value.end_byte())
        .is_some_and(|ancestor| ancestor.is_muted_statement())
}

/// The expression whose value `node` evaluates to as written: through an
/// assignment `x = v` or `x := v` the value `v` it assigns, and through
/// parentheses `(v)` the value `v` they hold. Hints of the same described
/// value give the same type.
fn described_value(mut node: SyntaxNode<'_>) -> SyntaxNode<'_> {
    loop {
        let inner = if node.has_binary_operator::<Token![=]>()
            || node.has_binary_operator::<Token![:=]>()
        {
            node.binary_parts().map(|parts| parts.right)
        } else if node.is_holder() {
            node.final_value_child()
        } else {
            None
        };
        match inner {
            Some(inner) => node = inner,
            None => return node,
        }
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

/// A binding target paired with the value and inferred label shown as an inlay hint.
struct AssignedValue {
    /// Extent of the binding target associated with the hint.
    target_range: TextRange,
    /// Source extent of the expression supplying the binding's value.
    value_range: TextRange,
    /// The range of the value [`described_value`] finds in the assigned one.
    described_range: TextRange,
    /// Type label displayed by the inlay hint.
    type_name: String,
    /// Whether the target receives one component of a parallel assignment.
    destructured: bool,
}

/// Source ranges already covered by assignment-related inlay hints.
#[derive(Default)]
struct AssignmentParts {
    /// Assignment extents already represented by a hint.
    assignments: Vec<TextRange>,
    /// Binding targets or documentation owners associated with this item.
    targets: Vec<TextRange>,
    /// Assigned-value extents already represented by a hint.
    values: Vec<TextRange>,
}

impl AssignmentParts {
    /// Whether an already shown assignment hint covers this expression or call.
    fn suppresses(&self, expression: TextRange, is_call: bool) -> bool {
        self.assignments.contains(&expression)
            || (!is_call && self.values.contains(&expression))
            || self
                .targets
                .iter()
                .any(|target| range_contains(*target, expression))
    }
}

/// Assignment values paired according to the assignment component positions.
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

/// Whether this node is a self describing value.
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

/// Unwraps parentheses to obtain the expression supplying a value.
fn parenthesized_value(mut node: SyntaxNode<'_>) -> SyntaxNode<'_> {
    while node.is_holder() {
        let Some(value) = node.final_value_child() else {
            break;
        };
        node = value;
    }
    node
}

/// Tests whether the outer source range covers the complete inner range.
fn range_contains(outer: TextRange, inner: TextRange) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::object_registry::{ObjectName, ObjectRegistry};

    /// The default settings with no place turned on.
    fn nowhere() -> InlayHintSettings {
        InlayHintSettings {
            after_line_end: false,
            after_cell: false,
            during_assignment: false,
            after_return: false,
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
        let options = InlayHintSettings {
            show_thing: true,
            everywhere: subexpressions,
            ..InlayHintSettings::default()
        };
        hints_with(text, &options, &ObjectRegistry::default())
    }

    /// The hints of `text` under `options`, with the library's types known,
    /// as their positions and labels.
    fn library_hints(text: &str, options: &InlayHintSettings) -> Vec<(Position, String)> {
        let registry = ObjectRegistry::load(crate::catalog::test_corpus());
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
            library_hints(
                source,
                &InlayHintSettings {
                    after_line_end: true,
                    ..nowhere()
                }
            ),
            [
                placed(pos!(1, 35), "-> Boolean"),
                placed(pos!(2, 17), "ZZ"),
                placed(pos!(7, 3), "ZZ"),
            ]
        );
        // The literal ending line 6 states its type.
        let literal_types = InlayHintSettings {
            show_literal: true,
            ..InlayHintSettings {
                after_line_end: true,
                ..nowhere()
            }
        };
        assert!(library_hints(source, &literal_types).contains(&placed(pos!(6, 2), "ZZ")));
    }

    #[test]
    fn line_ends_show_the_last_expression_complete_on_each_line() {
        // `x+` ends with the `x` before the operator, its hint after the
        // operator; the next line completes `x+x`. Comments are passed by.
        let source = "x = isPrime 7\nx or -- note\nx\n";
        assert_eq!(
            library_hints(
                source,
                &InlayHintSettings {
                    after_line_end: true,
                    ..nowhere()
                }
            ),
            [placed(pos!(1, 4), "Boolean"), placed(pos!(2, 1), "Boolean"),]
        );
    }

    #[test]
    fn semicolons_hide_the_values_they_discard_on_request() {
        // The cell and the call ending line 0 give the same hint, shown once.
        let source = "instance(3, ZZ);\nx = instance(3, ZZ);\n";
        let calls = InlayHintSettings {
            after_call: true,
            after_cell: true,
            ..nowhere()
        };
        assert_eq!(
            library_hints(source, &calls),
            [
                placed(pos!(0, 15), "-> Boolean"),
                placed(pos!(1, 19), "-> Boolean"),
            ]
        );
        let hidden = InlayHintSettings {
            hide_after_semicolon: true,
            ..calls
        };
        assert!(library_hints(source, &hidden).is_empty());
    }

    #[test]
    fn null_values_are_shown_only_on_request() {
        // A block ended by `;` is `null`.
        let source = "(1;)\n";
        let cells = InlayHintSettings {
            after_cell: true,
            ..nowhere()
        };
        assert!(library_hints(source, &cells).is_empty());
        assert_eq!(
            library_hints(
                source,
                &InlayHintSettings {
                    show_nothing: true,
                    ..cells
                }
            ),
            [placed(pos!(0, 4), "Nothing")]
        );
    }

    #[test]
    fn upper_closures_are_marked_unless_asked_not_to() {
        let marked = InlayHintSettings {
            after_cell: true,
            show_thing: true,
            ..nowhere()
        };
        let unmarked = InlayHintSettings {
            show_upper_closure: false,
            ..marked.clone()
        };

        assert_eq!(
            library_hints("print 3\n", &marked),
            [placed(pos!(0, 7), "-> ↑Thing")]
        );
        assert_eq!(
            library_hints("print 3\n", &unmarked),
            [placed(pos!(0, 7), "-> Thing")]
        );
    }

    #[test]
    fn cells_show_the_value_of_each_top_level_expression() {
        let source = concat!(
            "instance(3, ZZ)\n",
            "instance(3, ZZ);\n",
            "x = instance(3, ZZ)\n",
            "isPrime 7\n",
        );

        // A cell ended by `;` too, but not an assignment, whose binding
        // takes the hint.
        assert_eq!(
            library_hints(
                source,
                &InlayHintSettings {
                    after_cell: true,
                    ..nowhere()
                }
            ),
            [
                placed(pos!(0, 15), "-> Boolean"),
                placed(pos!(1, 15), "-> Boolean"),
                placed(pos!(3, 9), "-> Boolean"),
            ]
        );
    }

    #[test]
    fn calls_in_bracketed_form_leave_out_juxtapositions() {
        let source = "isPrime 7\ninstance(3, ZZ)\n";
        let options = InlayHintSettings {
            after_call: true,
            ..nowhere()
        };

        assert_eq!(
            library_hints(source, &options),
            [placed(pos!(1, 15), "-> Boolean")]
        );
    }

    #[test]
    fn brackets_show_parenthesized_values_only() {
        let source = concat!(
            "(isPrime 7)\n",
            "f(isPrime 7)\n",
            "(isPrime 7,)\n",
            "[isPrime 7]\n",
            "{isPrime 7}\n",
        );

        // Neither a call's arguments nor the collections get one.
        assert_eq!(
            library_hints(
                source,
                &InlayHintSettings {
                    after_brackets: true,
                    ..nowhere()
                }
            ),
            [placed(pos!(0, 11), "Boolean")]
        );
    }

    #[test]
    fn the_type_of_one_value_is_shown_once() {
        let everywhere_it_ends = InlayHintSettings {
            after_cell: true,
            after_line_end: true,
            after_call: true,
            after_brackets: true,
            show_repeated: true,
            ..nowhere()
        };
        let assigning = InlayHintSettings {
            during_assignment: true,
            ..everywhere_it_ends.clone()
        };

        // The assignment's hint stands for its value's cell, line end,
        // parentheses and call.
        assert_eq!(
            library_hints("x = (instance(3, ZZ))\n", &assigning),
            [placed(pos!(0, 1), "Boolean")]
        );
        assert_eq!(
            library_hints("x = instance(3, ZZ)\n", &everywhere_it_ends),
            [placed(pos!(0, 19), "-> Boolean")]
        );
        // A ring's presentation is one value, not a function's type.
        assert_eq!(
            library_hints(
                "R = QQ[a]\nS = R[x_0..y_2, z, w_(1,1)]\n",
                &everywhere_it_ends
            ),
            [
                placed(pos!(0, 9), "-> PolynomialRing"),
                placed(pos!(1, 27), "-> PolynomialRing"),
            ]
        );
        // The cell's hint stands for the line end past its `;`.
        assert_eq!(
            library_hints("isPrime 7;\n", &everywhere_it_ends),
            [placed(pos!(0, 9), "-> Boolean")]
        );
    }

    #[test]
    fn arguments_show_the_names_and_types_of_their_parameters() {
        let source = "f = method()\nf(ZZ, String) := (n, s) -> s\nf(1, \"a\")\n";
        let both = InlayHintSettings {
            arg_names: true,
            arg_types: true,
            ..nowhere()
        };
        let registry = ObjectRegistry::load(crate::catalog::test_corpus());
        let hints = hints_with(source, &both, &registry);

        assert_eq!(
            placed_labels(&hints),
            [placed(pos!(2, 2), "n: ZZ"), placed(pos!(2, 5), "s: String")]
        );
        assert!(hints.iter().all(|hint| {
            hint.kind == Some(InlayHintKind::PARAMETER) && hint.padding_right == Some(true)
        }));
        let names = InlayHintSettings {
            arg_types: false,
            ..both.clone()
        };
        assert_eq!(
            library_hints(source, &names),
            [placed(pos!(2, 2), "n:"), placed(pos!(2, 5), "s:")]
        );
        let types = InlayHintSettings {
            arg_names: false,
            ..both
        };
        assert_eq!(
            library_hints(source, &types),
            [placed(pos!(2, 2), "ZZ"), placed(pos!(2, 5), "String")]
        );
    }

    #[test]
    fn parameter_domains_are_not_marked_as_upper_closures() {
        // `Number` has subtypes, but a domain always takes them.
        assert_eq!(
            library_hints(
                "f = method()\nf Number := x -> x\nf(2)\n",
                &InlayHintSettings {
                    arg_names: true,
                    arg_types: true,
                    ..nowhere()
                }
            ),
            [placed(pos!(2, 2), "x: Number")]
        );
    }

    #[test]
    fn argument_names_come_from_the_called_lambda() {
        // A name the argument spells is not repeated, and a lambda taking
        // the whole sequence names no single argument.
        let source = "g = (a, b) -> a\ng(1, 2)\na = 1\ng(a, 3)\nh = x -> x\nh(1, 2)\n";
        assert_eq!(
            library_hints(
                source,
                &InlayHintSettings {
                    arg_names: true,
                    ..nowhere()
                }
            ),
            [
                placed(pos!(1, 2), "a:"),
                placed(pos!(1, 5), "b:"),
                placed(pos!(3, 5), "b:"),
            ]
        );
    }

    #[test]
    fn argument_types_of_library_calls_come_from_the_selected_signature() {
        // `instance` takes a `Thing` first, a type that says nothing.
        assert_eq!(
            library_hints(
                "instance(isPrime 7, ZZ)\n",
                &InlayHintSettings {
                    arg_types: true,
                    ..nowhere()
                }
            ),
            [placed(pos!(0, 17), "ZZ"), placed(pos!(0, 20), "Type")]
        );
    }

    #[test]
    fn excluded_calls_lose_their_hints_and_their_arguments_hints() {
        let options = InlayHintSettings {
            after_call: true,
            arg_types: true,
            excluded_calls: names(&["instance"]),
            ..nowhere()
        };

        // The call inside is not excluded, nor its argument's hint.
        assert_eq!(
            library_hints("instance(isPrime(7), ZZ)\n", &options),
            [placed(pos!(0, 17), "ZZ"), placed(pos!(0, 19), "-> Boolean")]
        );
    }

    #[test]
    fn hidden_types_are_never_shown() {
        let options = InlayHintSettings {
            hide_types: names(&["String"]),
            ..InlayHintSettings {
                during_assignment: true,
                ..nowhere()
            }
        };

        assert_eq!(
            library_hints("x = isPrime 7\ny = toString 3\n", &options),
            [placed(pos!(0, 1), "Boolean")]
        );
    }

    #[test]
    fn long_labels_are_cut_with_the_whole_type_in_the_tooltip() {
        let registry = ObjectRegistry::load(crate::catalog::test_corpus());
        let options = InlayHintSettings {
            max_label_length: Some(4),
            ..InlayHintSettings {
                during_assignment: true,
                ..nowhere()
            }
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
                    show_thing: unknown_types,
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
