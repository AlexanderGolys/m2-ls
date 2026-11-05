//! Type inlay hints derived from static document analysis.
//!
//! Each kind of place the settings turn on picks the values it annotates,
//! and one filter shared by all of them drops the hints the settings leave
//! out, such as those repeating what the source or a nearby hint already
//! says. Of the places giving the type of the same value, such as an
//! assignment, the cell it ends and the line it ends, only one is shown.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::iter;

use m2_syn::nodes::{
    AssignmentArgument, AssignmentList, Cell, Expr, ExprCollection, ExprControl, ExprLambda,
    ExprOp, ExprOpBin, ExprOpBinAdj, PackedContent, Symbol,
};
use m2_syn::visit::{self, Visit};
use m2_syn::{Lex, Spanned, Token, TokenStream, TokenTree};
use tower_lsp::lsp_types::{
    InlayHint, InlayHintKind, InlayHintLabel, InlayHintServerCapabilities, InlayHintTooltip, OneOf,
    Position, Range as TextRange,
};

use crate::analysis::{CallStaticFacts, InfixSyntax, MethodInstallation, Operator};
use crate::builtin_index::MethodSignature;
use crate::document::DocumentSnapshot;
use crate::meta::BindingRole;
use crate::object_registry::{ObjectName, ObjectRegistry};
use crate::settings::{ArgumentNameSettings, HintBracket, InlayHintSettings};
use crate::source::{ByteRange, SourceNavigation, SyntaxSource};
use crate::syntax::{
    AssignmentListSyntax, AssignmentOperator, AssignmentSyntax, AssignmentTarget, BoundSymbols,
    ExpressionSyntax, LambdaSyntax,
};
use crate::typesystem::InferredType;
use crate::util::TextRangeExt;

/// What a call-result hint starts with, before the type the call returns.
const CALL_RESULT_PREFIX: &str = "-> ";
/// The type of `null`: Macaulay2 prints nothing for a cell of this value.
const NOTHING_TYPE: &str = "Nothing";

/// The kind of place a hint is put at. Of the hints giving the type of the
/// same value, the one of the earliest kind is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HintSource {
    /// An explicit holder request, kept at the parentheses rather than moved to a binding.
    RequestedHolder,
    /// After the name an assignment binds.
    Assignment,
    /// After the value a function returns.
    Return,
    /// After a selected collection delimiter or an explicitly held value.
    Delimited,
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

    if options.hide_sequential {
        let mut retained_positions = HashSet::new();
        hints.reverse();
        hints.retain(|hint| {
            hint.kind != Some(InlayHintKind::TYPE)
                || retained_positions.insert((hint.position.line, hint.position.character))
        });
        hints.reverse();
    }

    hints
}

/// The literal symbols whose spelling states their type.
const LITERAL_SYMBOLS: [&str; 3] = ["true", "false", "null"];

/// What the hints of one request are computed from: the document, the types
/// analysis inferred for its expressions, and the settings choosing which
/// are shown.
struct HintContext<'request, 'a> {
    /// Immutable document snapshot being queried.
    document: &'a DocumentSnapshot,
    /// Positioned object and type facts available to this request.
    knowledge: &'request ObjectRegistry,
    /// Options accepted by the callable, or settings governing this request.
    options: &'request InlayHintSettings,
    /// The part of the document the client asks hints for.
    range: TextRange,
    /// Every typed expression with its inferred type, in postorder.
    typed: Vec<(&'a Expr, InferredType)>,
    /// Where each typed expression is in `typed`, by identity.
    typed_index: HashMap<*const Expr, usize>,
    /// The syntax facts the hints are placed by.
    syntax: HintSyntax<'a>,
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

/// The syntax facts of a document that decide where hints go, collected in
/// one visit.
#[derive(Default)]
struct HintSyntax<'a> {
    /// Every function literal, to find the parameters a call passes to.
    lambdas: Vec<&'a ExprLambda>,
    /// Every assignment expression.
    assignments: Vec<&'a Expr>,
    /// The argument expressions of applications, by identity.
    call_arguments: HashSet<*const Expr>,
    /// Bytes of the expressions a `;` discards the value of.
    discarded: Vec<ByteRange>,
    /// The value each cell ends with, as Macaulay2's `oN : T` line reads it.
    cell_values: Vec<&'a Expr>,
    /// The `return`s, by identity, whose value some function receives.
    received_returns: HashSet<*const ExprControl>,
}

/// Visitor collecting [`HintSyntax`].
struct HintSyntaxVisitor<'a, 'document> {
    /// Source coordinates of the visited revision.
    document: &'document DocumentSnapshot,
    /// Facts collected so far.
    syntax: HintSyntax<'a>,
}

impl<'a> HintSyntaxVisitor<'a, '_> {
    /// Records the final value of discarded `content` as discarded.
    fn discard(&mut self, content: &PackedContent) {
        let value = match content {
            PackedContent::Expr(expression) => Some(expression.as_ref()),
            PackedContent::Pack(pack) => {
                pack.pack
                    .iter()
                    .last()
                    .and_then(|component| match component {
                        m2_syn::nodes::Component::Expr(expression) => Some(expression.as_ref()),
                        m2_syn::nodes::Component::Empty(_) => None,
                    })
            }
        };
        if let Some(bytes) = value.and_then(|value| self.document.syntax_bytes(value)) {
            self.syntax.discarded.push(bytes);
        }
    }
}

impl<'a> Visit<'a> for HintSyntaxVisitor<'a, '_> {
    fn visit_cell(&mut self, cell: &'a Cell) {
        if let PackedContent::Expr(value) = &cell.content {
            self.syntax.cell_values.push(value);
        }
        if cell.semicolon.is_some() {
            self.discard(&cell.content);
        }
        visit::visit_cell(self, cell);
    }

    fn visit_statement(&mut self, statement: &'a m2_syn::nodes::Statement) {
        self.discard(&statement.content);
        visit::visit_statement(self, statement);
    }

    fn visit_expr(&mut self, expression: &'a Expr) {
        match expression {
            Expr::Lambda(lambda) => self.syntax.lambdas.push(lambda),
            Expr::Assign(_) => self.syntax.assignments.push(expression),
            Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call))) => {
                self.syntax
                    .call_arguments
                    .insert(std::ptr::from_ref(call.right_operand.as_ref()));
            }
            _ => {}
        }
        visit::visit_expr(self, expression);
    }
}

impl<'request, 'a> HintContext<'request, 'a> {
    /// Constructs this value from the supplied dependencies and initial state.
    fn new(
        document: &'a DocumentSnapshot,
        knowledge: &'request ObjectRegistry,
        options: &'request InlayHintSettings,
        range: TextRange,
    ) -> Self {
        let typed = document
            .analysis()
            .expression_types(document.syntax(), document, knowledge);
        let typed_index = typed
            .iter()
            .enumerate()
            .map(|(index, (expression, _))| (std::ptr::from_ref(*expression), index))
            .collect();
        let mut visitor = HintSyntaxVisitor {
            document,
            syntax: HintSyntax::default(),
        };
        document.syntax().visit(&mut visitor);
        let mut syntax = visitor.syntax;
        syntax.received_returns = document
            .analysis()
            .control_transfers(
                document
                    .syntax()
                    .cells()
                    .iter()
                    .filter_map(crate::node_metadata::ParsedCell::syntax),
                document,
                knowledge,
            )
            .into_iter()
            .filter(|transfer| transfer.target.is_some())
            .map(|transfer| std::ptr::from_ref(transfer.transfer))
            .collect();
        let mut context = Self {
            document,
            knowledge,
            options,
            range,
            typed,
            typed_index,
            syntax,
            assignment_parts: AssignmentParts::default(),
            opaque_self_describing_values: Vec::new(),
        };
        context.assignment_parts = context.assignment_parts();
        context.opaque_self_describing_values = context
            .self_describing_assignment_values()
            .into_iter()
            .filter(|value| Self::literal_is_opaque(value))
            .filter_map(|value| document.range_for_syntax(value))
            .collect();
        context
    }

    /// Whether literal syntax also makes all nested type hints redundant.
    /// Computed collection elements and lambda bodies retain their own hints.
    fn literal_is_opaque(value: &Expr) -> bool {
        match value.unparenthesized() {
            Some(Expr::Lambda(_)) => false,
            Some(collection @ Expr::Collection(_)) => {
                collection.collection_elements().is_some_and(|elements| {
                    elements
                        .into_iter()
                        .all(|element| element.is_none_or(Self::literal_is_opaque))
                })
            }
            _ => is_literal_value(value),
        }
    }

    /// The hints of every kind of place the settings turn on.
    fn placed_hints(&self) -> Vec<PlacedHint> {
        let options = self.options;
        let mut hints = Vec::new();
        if options.types_enabled && options.during_assignment {
            hints.extend(self.binding_type_hints());
        }
        if options.types_enabled && options.after_return {
            hints.extend(self.lambda_return_type_hints());
        }
        if options.types_enabled && options.after_cell {
            hints.extend(self.value_hints(
                self.syntax.cell_values.iter().copied(),
                HintSource::Cell,
                HintPlacement::AfterValue,
            ));
        }
        if options.types_enabled && options.after_call {
            hints.extend(
                self.value_hints(
                    self.typed_expressions()
                        .filter(|value| is_bracketed_call(value)),
                    HintSource::Call,
                    HintPlacement::AfterValue,
                ),
            );
        }
        if options.types_enabled && (options.after_brackets || !options.bracket_kinds.is_empty()) {
            hints.extend(
                self.value_hints(
                    self.typed_expressions()
                        .filter(|value| self.has_hint_brackets(value)),
                    HintSource::Delimited,
                    HintPlacement::AfterValue,
                ),
            );
        }
        if options.types_enabled && options.after_line_end {
            hints.extend(self.line_end_hints());
        }
        if options.arg_names || options.types_enabled && options.arg_types {
            hints.extend(self.parameter_hints().into_iter().map(|hint| PlacedHint {
                value: None,
                source: HintSource::Argument,
                hint,
            }));
        }
        if options.types_enabled && options.everywhere {
            hints.extend(self.value_hints(
                self.typed_expressions(),
                HintSource::Everywhere,
                HintPlacement::AfterValue,
            ));
            hints.extend(self.parameter_binding_hints());
        }
        hints
    }

    /// Whether this exact delimiter form is selected outside call arguments.
    fn has_hint_brackets(&self, value: &Expr) -> bool {
        let kind = match value {
            Expr::Collection(ExprCollection::Sequence(_)) => HintBracket::Parentheses,
            Expr::Collection(ExprCollection::Array(_)) => HintBracket::Array,
            Expr::Collection(ExprCollection::List(_)) => HintBracket::List,
            Expr::Collection(ExprCollection::AngleBar(_)) => HintBracket::AngleBar,
            Expr::Holder(_) => HintBracket::Holder,
            _ => return false,
        };
        (self.options.bracket_kinds.contains(&kind)
            || self.options.after_brackets && kind == HintBracket::Holder)
            && !self
                .syntax
                .call_arguments
                .contains(&std::ptr::from_ref(value))
    }

    /// The expressions analysis inferred a type for.
    fn typed_expressions(&self) -> impl Iterator<Item = &'a Expr> + '_ {
        self.typed.iter().map(|(expression, _)| *expression)
    }

    /// The inferred type of `expression`.
    fn inferred_type(&self, expression: &Expr) -> Option<&InferredType> {
        let index = *self.typed_index.get(&std::ptr::from_ref(expression))?;
        Some(&self.typed[index].1)
    }

    /// The hints of each of `values` placed at `placement` that the filters
    /// keep.
    fn value_hints(
        &self,
        values: impl IntoIterator<Item = &'a Expr>,
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
        value: &'a Expr,
        source: HintSource,
        placement: HintPlacement,
    ) -> Option<PlacedHint> {
        let inferred_type = self.inferred_type(value)?;
        let value_range = self.document.range_for_syntax(value)?;
        let requested_holder = source == HintSource::Delimited && matches!(value, Expr::Holder(_));
        let repeated = || {
            self.assignment_parts
                .suppresses(value_range, is_application(value))
        };
        let stated_by_literal = || {
            is_literal_value(value)
                || self
                    .opaque_self_describing_values
                    .iter()
                    .any(|opaque| range_contains(*opaque, value_range))
        };
        if !range_contains(self.range, value_range)
            || (self.options.hide_after_semicolon && self.is_discarded(value))
            || self.is_excluded_call(value)
            || self.type_is_stated_by_installation(value)
            || (!requested_holder && !self.options.show_repeated && repeated())
            || (!requested_holder
                && (!self.options.show_literal || self.options.hide_trivial)
                && stated_by_literal())
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
            value: self.document.range_for_syntax(described_value(value)),
            source: if requested_holder {
                HintSource::RequestedHolder
            } else {
                source
            },
            hint: self.type_hint(position, expression_label(value, type_name)),
        })
    }

    /// Whether nothing but a `;` follows `value` in the statement it ends,
    /// so its value is discarded.
    fn is_discarded(&self, value: &Expr) -> bool {
        self.document.syntax_bytes(value).is_some_and(|value| {
            self.syntax
                .discarded
                .iter()
                .any(|statement| statement.start <= value.start && statement.end == value.end)
        })
    }

    /// The parameters each call passes its arguments to, before the
    /// arguments, as `f(Number x: 2)`; none for a call whose parameters do
    /// not match its arguments one to one.
    fn parameter_hints(&self) -> Vec<InlayHint> {
        let mut hints = self
            .typed_expressions()
            .filter(|value| !self.is_excluded_call(value))
            .filter_map(|value| match value {
                Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call))) => Some(call),
                _ => None,
            })
            .flat_map(|call| {
                let arguments = call.right_operand.arguments();
                let parameters = self.called_parameters(call);
                if arguments.len() != parameters.len() {
                    return Vec::new();
                }
                let names_allowed = !self.options.argument_names.require_bracket
                    || call.right_operand.delimiter().is_some();
                iter::zip(arguments, parameters)
                    .filter_map(|(argument, mut parameter)| {
                        if !names_allowed {
                            parameter.name = None;
                        }
                        self.parameter_hint(argument?, parameter)
                    })
                    .collect()
            })
            .collect::<Vec<_>>();
        if !self.options.hide_operator_calls {
            hints.extend(
                self.typed_expressions()
                    .filter_map(|value| match value {
                        Expr::Op(operation)
                            if !matches!(operation, ExprOp::Bin(ExprOpBin::Adj(_))) =>
                        {
                            Some(operation)
                        }
                        _ => None,
                    })
                    .flat_map(|operation| self.operator_parameter_hints(operation)),
            );
        }
        hints
    }

    /// The parameters `call` passes its arguments to, as far as they are
    /// known: of the one local method installation or library signature the
    /// argument types select, or of the lambda a local function is.
    fn called_parameters(&self, call: &ExprOpBinAdj) -> Vec<CalledParameter> {
        let Expr::Symbol(head) = call.left_operand.as_ref() else {
            return Vec::new();
        };
        let name = head.text();
        let analysis = self.document.analysis();
        let Some(position) = self.document.position_for_syntax(head) else {
            return Vec::new();
        };
        let view = self.knowledge.at(position);
        let Some(function) = analysis.function_at(name, position) else {
            let facts = analysis.infer_call_static_facts(call, self.document, &view);
            let Some(signature) = analysis
                .infer_external_call_signature_facts(
                    &ObjectName::new(name),
                    &facts,
                    position,
                    &view,
                )
                .and_then(|facts| facts.pinned)
            else {
                return Vec::new();
            };
            return self.external_parameters(&signature);
        };
        if !function.is_method_function() {
            let value_range = analysis
                .get_binding_at(name, position)
                .and_then(|binding| binding.state.value_range);
            let lambda = self.syntax.lambdas.iter().find(|lambda| {
                value_range.is_some() && value_range == self.document.range_for_syntax(**lambda)
            });
            return lambda
                .and_then(|lambda| self.parameter_names(lambda))
                .unwrap_or_default()
                .into_iter()
                .map(|name| CalledParameter {
                    name: Some(name),
                    domain: None,
                })
                .collect();
        }
        let installations =
            analysis.local_call_installations(function, call, position, self.document, &view);
        let [installation] = installations.as_slice() else {
            return Vec::new();
        };
        self.installation_parameters(installation)
    }

    /// Names supplied by a source lambda, respecting variadic-name filtering.
    fn parameter_names(&self, lambda: &ExprLambda) -> Option<Vec<String>> {
        (!self.options.argument_names.ignore_variadic || !matches!(lambda, ExprLambda::Variadic(_)))
            .then(|| {
                lambda
                    .bound_symbols()
                    .iter()
                    .map(|symbol| symbol.text().to_string())
                    .collect()
            })
    }

    /// Parameter names and domains supplied by one selected source installation.
    fn installation_parameters(&self, installation: &MethodInstallation) -> Vec<CalledParameter> {
        let analysis = self.document.analysis();
        let names = self
            .syntax
            .lambdas
            .iter()
            .find(|lambda| {
                analysis
                    .installation_receiving_arguments(lambda, self.document)
                    .is_some_and(|receiving| receiving.id == installation.id)
            })
            .and_then(|lambda| self.parameter_names(lambda))
            .filter(|names| names.len() == installation.method.domain.len());
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

    /// Catalog domain types, without inventing parameter names absent from metadata.
    fn external_parameters(&self, signature: &MethodSignature) -> Vec<CalledParameter> {
        signature
            .domain
            .iter()
            .map(|object| CalledParameter {
                name: None,
                domain: self
                    .knowledge
                    .object(object)
                    .map(|record| record.name.clone()),
            })
            .collect()
    }

    /// Parameter labels on typed operator operands, independently of expression type hints.
    fn operator_parameter_hints(&self, operation: &ExprOp) -> Vec<InlayHint> {
        let Some(range) = self.document.range_for_syntax(operation) else {
            return Vec::new();
        };
        if self
            .assignment_parts
            .targets
            .iter()
            .any(|target| range_contains(*target, range))
        {
            return Vec::new();
        }
        let Some(operator) = Operator::of_operation(operation) else {
            return Vec::new();
        };
        let operands = match operation {
            ExprOp::Bin(binary) => {
                let Some(operands) = binary.infix_operands() else {
                    return Vec::new();
                };
                vec![operands.left, operands.right]
            }
            ExprOp::Prefix(prefix) => vec![prefix.operand.as_ref()],
            ExprOp::Postfix(postfix) => vec![postfix.operand.as_ref()],
        };
        let facts = CallStaticFacts {
            argument_types: operands
                .iter()
                .map(|operand| {
                    self.inferred_type(operand)
                        .cloned()
                        .unwrap_or_else(InferredType::unknown)
                })
                .collect(),
            literal_options: Vec::new(),
        };
        let analysis = self.document.analysis();
        let view = self.knowledge.at(range.start);
        let installations = analysis.operator_call_installations(
            &operator,
            &facts.argument_types,
            range.start,
            &view,
        );
        let parameters = match installations.as_slice() {
            [installation] => self.installation_parameters(installation),
            [] => analysis
                .infer_external_call_signature_facts(&operator.token, &facts, range.start, &view)
                .and_then(|facts| facts.pinned)
                .map_or_else(Vec::new, |signature| self.external_parameters(&signature)),
            _ => return Vec::new(),
        };
        if operands.len() != parameters.len() {
            return Vec::new();
        }
        iter::zip(operands, parameters)
            .filter_map(|(operand, mut parameter)| {
                if self.options.argument_names.require_bracket {
                    parameter.name = None;
                }
                self.parameter_hint(operand, parameter)
            })
            .collect()
    }

    /// The hint before `argument` of the `parameter` it is passed to, of
    /// what the settings show of it: `T x:`, `x:` or `T`. A name the
    /// argument already spells is left out.
    fn parameter_hint(&self, argument: &Expr, parameter: CalledParameter) -> Option<InlayHint> {
        let position = self.document.position_for_syntax(argument)?;
        if !self.range.contains_position(position) {
            return None;
        }
        let name = parameter.name.filter(|name| {
            self.options.arg_names && Some(name.as_str()) != self.document.text_for_syntax(argument)
        });
        let type_name = parameter
            .domain
            .filter(|_| self.options.types_enabled && self.options.arg_types)
            .and_then(|domain| {
                self.displayed_type(
                    position,
                    &InferredType::upward_from_id(domain),
                    TypedSubject::ParameterDomain,
                )
            });
        let displayed_name = name
            .as_deref()
            .map(|name| self.options.argument_names.display_name(name));
        let tooltip = name
            .filter(|name| displayed_name.as_ref() != Some(name))
            .map(InlayHintTooltip::String);
        let label = match (displayed_name, type_name) {
            (Some(name), Some(type_name)) => format!("{type_name} {name}:"),
            (Some(name), None) => format!("{name}:"),
            (None, Some(type_name)) => type_name,
            (None, None) => return None,
        };
        Some(InlayHint {
            kind: Some(InlayHintKind::PARAMETER),
            padding_left: None,
            padding_right: Some(true),
            position,
            label: label.into(),
            text_edits: None,
            tooltip,
            data: None,
        })
    }

    /// The hints at the ends of lines, each of the last expression complete
    /// on its line: the outermost of those ending last there, unless an
    /// expression starting after it on the line goes on below, as one an
    /// opening bracket starts.
    fn line_end_hints(&self) -> Vec<PlacedHint> {
        let line_of = |byte| self.document.position_for_byte(byte).line;
        let mut last_complete = HashMap::<u32, (&'a Expr, ByteRange)>::new();
        let mut last_continued_start = HashMap::<u32, usize>::new();
        for value in self.typed_expressions() {
            let Some(bytes) = self.document.syntax_bytes(value) else {
                continue;
            };
            let (start_line, end_line) = (line_of(bytes.start), line_of(bytes.end));
            if start_line < end_line {
                let start = last_continued_start.entry(start_line).or_default();
                *start = (*start).max(bytes.start);
            }
            let ends_later =
                |other: &ByteRange| (bytes.end, bytes.len()) > (other.end, other.len());
            last_complete
                .entry(end_line)
                .and_modify(|last| {
                    if ends_later(&last.1) {
                        *last = (value, bytes.clone());
                    }
                })
                .or_insert((value, bytes.clone()));
        }
        let content_ends = self.line_content_ends();
        last_complete
            .into_iter()
            .filter(|(line, (_, bytes))| {
                last_continued_start
                    .get(line)
                    .is_none_or(|start| *start < bytes.end)
            })
            .filter_map(|(line, (value, bytes))| {
                let line_end = content_ends.get(&line).copied().unwrap_or(bytes.end);
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
        /// Records where `syntax` ends as the end of its line's code, when it
        /// ends after what the line held so far.
        fn record_end(
            syntax: &dyn Spanned,
            document: &DocumentSnapshot,
            ends: &mut HashMap<u32, usize>,
        ) {
            if let Some(bytes) = document.syntax_bytes(syntax) {
                let end = ends
                    .entry(document.position_for_byte(bytes.end).line)
                    .or_default();
                *end = (*end).max(bytes.end);
            }
        }
        /// Records the end of each token of `stream` on its line.
        fn record(
            stream: TokenStream,
            document: &DocumentSnapshot,
            ends: &mut HashMap<u32, usize>,
        ) {
            for token in stream {
                match token {
                    TokenTree::Group(group) => {
                        record_end(&group.delimiter().opening_span(), document, ends);
                        if let Some(closing) = group.delimiter().closing_span() {
                            record_end(&closing, document, ends);
                        }
                        record(group.stream().clone(), document, ends);
                    }
                    token => record_end(&token, document, ends),
                }
            }
        }
        let mut ends = HashMap::<u32, usize>::new();
        for cell in self
            .document
            .syntax()
            .cells()
            .iter()
            .filter_map(crate::node_metadata::ParsedCell::syntax)
        {
            record(cell.to_token_stream(), self.document, &mut ends);
        }
        ends
    }

    /// Whether `value` calls a function the settings exclude.
    fn is_excluded_call(&self, value: &Expr) -> bool {
        match value {
            Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call))) => call
                .left_operand
                .symbol_name()
                .is_some_and(|name| self.options.excluded_calls.contains(name)),
            _ => false,
        }
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
                    .chain(iter::once(ArgumentNameSettings::TRUNCATION_MARK))
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
        self.typed_expressions()
            .filter_map(|value| match value {
                Expr::Control(transfer @ ExprControl::Return(node))
                    if self
                        .syntax
                        .received_returns
                        .contains(&std::ptr::from_ref(transfer)) =>
                {
                    node.value.as_deref().map(|value| (value, None))
                }
                Expr::Lambda(lambda) => {
                    let installation = self
                        .document
                        .analysis()
                        .installation_receiving_arguments(lambda, self.document);
                    let constructed = installation
                        .filter(|installation| {
                            installation.method.head.name().name() == <Token![new]>::SPELLING
                        })
                        .and_then(|installation| installation.method.codomain.as_ref());
                    lambda_final_value(lambda).map(|value| (value, constructed))
                }
                _ => None,
            })
            .filter(|(value, constructed)| {
                constructed.is_some()
                    || self.options.show_literal && !self.options.hide_trivial
                    || !is_literal_value(value)
            })
            .filter_map(|(value, constructed)| {
                let value_range = self.document.range_for_syntax(value)?;
                if !self.range.contains_position(value_range.end) {
                    return None;
                }
                let type_name = match constructed {
                    Some(class) => self.displayed_type(
                        value_range.start,
                        &InferredType::exact_from_id(class.clone()),
                        TypedSubject::Value,
                    )?,
                    None => self.inferred_type_label(value)?,
                };
                Some(PlacedHint {
                    value: self.document.range_for_syntax(described_value(value)),
                    source: HintSource::Return,
                    hint: self.type_hint(value_range.end, expression_label(value, type_name)),
                })
            })
            .collect()
    }

    /// The label of the inferred type of `value`, when it is shown.
    fn inferred_type_label(&self, value: &Expr) -> Option<String> {
        self.displayed_type(
            self.document.position_for_syntax(value)?,
            self.inferred_type(value)?,
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
            .self_describing_assignment_values()
            .into_iter()
            .filter_map(|value| self.document.range_for_syntax(value))
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
                if (!self.options.show_literal || self.options.hide_trivial)
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

    /// Types of lambda parameter bindings when hints are requested everywhere.
    fn parameter_binding_hints(&self) -> Vec<PlacedHint> {
        self.document
            .analysis()
            .bindings()
            .filter(|binding| binding.role == BindingRole::Parameter)
            .filter_map(|binding| {
                let state = binding.states.first()?;
                if state.inferred_type.is_some() || !self.range.contains_position(state.span.end) {
                    return None;
                }
                let inferred = InferredType::unknown();
                let label =
                    self.displayed_type(state.span.start, &inferred, TypedSubject::Value)?;
                Some(PlacedHint {
                    value: Some(state.span),
                    source: HintSource::Everywhere,
                    hint: self.type_hint(state.span.end, label),
                })
            })
            .collect()
    }

    /// Whether `value` names a parameter whose type its method installation
    /// states.
    fn type_is_stated_by_installation(&self, value: &Expr) -> bool {
        let Expr::Symbol(symbol) = value else {
            return false;
        };
        self.document
            .position_for_syntax(symbol)
            .and_then(|position| {
                self.document
                    .analysis()
                    .get_binding_at(symbol.text(), position)
            })
            .is_some_and(|binding| {
                binding.role == BindingRole::Parameter && binding.state.inferred_type.is_some()
            })
    }

    /// Target and value operands extracted from supported assignment syntax.
    fn assignment_parts(&self) -> AssignmentParts {
        let mut parts = AssignmentParts::default();
        for &expression in &self.syntax.assignments {
            let Expr::Assign(assignment) = expression else {
                continue;
            };
            let assigned = assignment.parts();
            parts
                .assignments
                .extend(self.document.range_for_syntax(expression));
            if let Some(target) = assigned.target.bytes(self.document) {
                parts.targets.push(self.document.range_for_bytes(target));
            }
            parts
                .values
                .extend(self.document.range_for_syntax(assigned.value));
        }
        parts
    }

    /// The parts of the `=` and `:=` assignments.
    fn plain_assignments(&self) -> impl Iterator<Item = crate::syntax::AssignmentParts<'a>> + '_ {
        self.syntax.assignments.iter().filter_map(|expression| {
            let Expr::Assign(assignment) = expression else {
                return None;
            };
            let parts = assignment.parts();
            matches!(
                parts.operator,
                AssignmentOperator::Global | AssignmentOperator::Local
            )
            .then_some(parts)
        })
    }

    /// Target/value pairs eligible for assignment type hints.
    fn assigned_values(&self) -> Vec<AssignedValue> {
        let mut values = Vec::new();
        for parts in self.plain_assignments() {
            let (pairs, destructured) = match parts.target {
                AssignmentTarget::Symbol(symbol) => (vec![(symbol, parts.value)], false),
                AssignmentTarget::List(targets) => {
                    (paired_assignment_values(targets, parts.value), true)
                }
                _ => continue,
            };
            for (target, value) in pairs {
                let Some(type_name) = self.inferred_type_label(value) else {
                    continue;
                };
                let (Some(target_range), Some(value_range), Some(described_range)) = (
                    self.document.range_for_syntax(target),
                    self.document.range_for_syntax(value),
                    self.document.range_for_syntax(described_value(value)),
                ) else {
                    continue;
                };
                values.push(AssignedValue {
                    target_range,
                    value_range,
                    described_range,
                    type_name,
                    destructured,
                });
            }
        }
        values
    }

    /// Values assigned to a name whose literal syntax already communicates
    /// the displayed type.
    fn self_describing_assignment_values(&self) -> Vec<&'a Expr> {
        self.syntax
            .assignments
            .iter()
            .filter_map(|expression| {
                let Expr::Assign(assignment) = expression else {
                    return None;
                };
                let parts = assignment.parts();
                (matches!(parts.target, AssignmentTarget::Symbol(_))
                    && is_literal_value(parts.value))
                .then_some(parts.value)
            })
            .collect()
    }
}

/// Final expression contributing the lambda's return value.
fn lambda_final_value(lambda: &ExprLambda) -> Option<&Expr> {
    let value = lambda.body().unparenthesized()?;
    (!matches!(value, Expr::Control(ExprControl::Return(_)))).then_some(value)
}

/// Whether `value` applies a function.
fn is_application(value: &Expr) -> bool {
    matches!(value, Expr::Op(ExprOp::Bin(ExprOpBin::Adj(_))))
}

/// A call whose arguments are bracketed, as `f(x)`, `f()` or `QQ[x]`, not
/// one by juxtaposition, as `f x`.
fn is_bracketed_call(value: &Expr) -> bool {
    matches!(
        value,
        Expr::Op(ExprOp::Bin(ExprOpBin::Adj(call)))
            if matches!(call.right_operand.as_ref(), Expr::Collection(_) | Expr::Holder(_))
    )
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
fn expression_label(expression: &Expr, type_name: String) -> String {
    if is_application(expression) {
        format!("{CALL_RESULT_PREFIX}{type_name}")
    } else {
        type_name
    }
}

/// The expression whose value `value` evaluates to as written: through an
/// assignment `x = v` or `x := v` the value `v` it assigns, and through
/// parentheses `(v)` the value `v` they hold. Hints of the same described
/// value give the same type.
fn described_value(mut value: &Expr) -> &Expr {
    loop {
        let inner = match value {
            Expr::Assign(assignment) => {
                let parts = assignment.parts();
                matches!(
                    parts.operator,
                    AssignmentOperator::Global | AssignmentOperator::Local
                )
                .then_some(parts.value)
            }
            Expr::Holder(holder) => holder.value.as_deref(),
            _ => None,
        };
        match inner {
            Some(inner) => value = inner,
            None => return value,
        }
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

/// The symbols of a parallel assignment's `targets` paired with the values
/// `value` assigns them, by position.
fn paired_assignment_values<'a>(
    targets: &'a AssignmentList,
    value: &'a Expr,
) -> Vec<(&'a Symbol, &'a Expr)> {
    let targets = targets.targets();
    let Some(value) = value.unparenthesized() else {
        return Vec::new();
    };
    let pair = |target: &'a AssignmentArgument, value: &'a Expr| match target {
        AssignmentArgument::Symbol(symbol) => vec![(symbol, value)],
        AssignmentArgument::List(nested) => paired_assignment_values(nested, value),
        _ => Vec::new(),
    };
    if let Some(values) = value.collection_elements() {
        if targets.len() != values.len() {
            return Vec::new();
        }
        return targets
            .into_iter()
            .zip(values)
            .filter_map(|(target, value)| Some(pair(target, value?)))
            .flatten()
            .collect();
    }
    match targets.as_slice() {
        [target] => pair(target, value),
        _ => Vec::new(),
    }
}

/// Whether an expression is a directly written value: a scalar, collection,
/// lambda, or explicit `new` construction. Holders preserve this classification.
fn is_literal_value(value: &Expr) -> bool {
    match described_value(value).unparenthesized() {
        Some(Expr::Literal(_) | Expr::Lambda(_) | Expr::New(_) | Expr::Collection(_)) => true,
        Some(Expr::Symbol(symbol)) => LITERAL_SYMBOLS.contains(&symbol.text()),
        _ => false,
    }
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
            after_call: false,
            arg_names: false,
            bracket_kinds: HashSet::new(),
            hide_trivial: false,
            hide_sequential: false,
            hide_operator_calls: false,
            excluded_calls: HashSet::new(),
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

        // An explicit holder takes precedence over the assignment, cell,
        // line end and call without repeating the value's type.
        assert_eq!(
            library_hints("x = (instance(3, ZZ))\n", &assigning),
            [placed(pos!(0, 21), "Boolean")]
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
            [placed(pos!(2, 2), "ZZ n:"), placed(pos!(2, 5), "String s:")]
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
                    argument_names: crate::settings::ArgumentNameSettings {
                        ignore_variadic: false,
                        ..Default::default()
                    },
                    ..nowhere()
                }
            ),
            [placed(pos!(2, 2), "Number x:")]
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
        assert!(calm.is_empty());
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
