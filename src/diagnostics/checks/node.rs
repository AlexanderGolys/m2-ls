//! The node occasion: diagnostics spawned while visiting each syntax node.

use m2_syn::nodes::{
    ExprIf as IfStatement, ExprLambda as LambdaExpression, ExprQuote as QuoteExpression,
    ExprWhile as WhileLoop, Symbol,
};
use m2_syn::Token;

use super::{DiagnosticSink, NodeContext};
use crate::analysis::{
    assigned_lambda, parenthesized_value, CallableHeadKind, ControlTransferTarget,
    MethodCodomainEdit, MethodHead, OptionDeclaration, RingVariableError, TypeChecker,
};
use crate::diagnostics::rewrites::{
    ambiguous_float_member_access_rewrite, coalescence_rewrite, simplification_of, Coalescence,
};
use crate::diagnostics::{catalog, Finding};
use crate::node_metadata::{matches_token, SyntaxNode};
use crate::object_registry::{ObjectKnowledge, ObjectName};
use crate::source::SourceNavigation;
use crate::typesystem::{SubtypeEvidence, TypeKnowledge, TypeRole};

/// Spawns every diagnostic found at the context's node.
pub(super) fn spawn(context: &NodeContext, sink: &mut DiagnosticSink) {
    syntax_error(context, sink);
    missing_node(context, sink);
    ambiguous_float_member_access(context, sink);
    multiple_assignment_targets(context, sink);
    colon_equal_part_assignment(context, sink);
    parallel_assignment(context, sink);
    protected_assignment(context, sink);
    ring_variable_selection(context, sink);
    option_key_convention(context, sink);
    prefer_coalescence(context, sink);
    ring_variable_naming(context, sink);
    install_needs_colon_equals(context, sink);
    protect_computed_symbol(context, sink);
    invalid_control_transfer(context, sink);
    explicit_install_required(context, sink);
    condition_type(context, sink);
    install_codomain(context, sink);
    call_domain(context, sink);
}

fn syntax_error(context: &NodeContext, sink: &mut DiagnosticSink) {
    let node = context.node;
    if node.is_error() {
        if matches!(
            node.parse_error(),
            Some(m2_syn::NativeParseError::InvalidAssignmentTarget { .. })
        ) {
            sink.spawn(
                &catalog::MULTIPLE_ASSIGNMENT_TARGETS,
                Finding::new(
                    context.node_range(),
                    "This parallel assignment contains a target that cannot be assigned",
                ),
            );
            return;
        }
        for range in node.error_ranges() {
            sink.spawn(
                &catalog::SYNTAX_ERROR,
                Finding::new(context.source.range_for_bytes(range), "Syntax error"),
            );
        }
    }
}

fn missing_node(context: &NodeContext, sink: &mut DiagnosticSink) {
    if context.node.is_missing() {
        sink.spawn(
            &catalog::MISSING_NODE,
            Finding::new(
                context.node_range(),
                format!("Missing: {}", context.node.syntax_label()),
            ),
        );
    }
}

fn ambiguous_float_member_access(context: &NodeContext, sink: &mut DiagnosticSink) {
    let Some(replacement) = ambiguous_float_member_access_rewrite(context.node) else {
        return;
    };
    sink.spawn(
        &catalog::AMBIGUOUS_FLOAT_MEMBER_ACCESS,
        Finding::new(
            context.node_range(),
            format!(
                "This is parsed as application to a float literal; use `{replacement}` for member access"
            ),
        ),
    );
}

/// Which operator a parallel assignment uses; it decides what an operator
/// target means (an installation under `:=`, an assignment under `=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParallelAssignment {
    /// `(...) := ...`
    Local,
    /// `(...) = ...`
    Global,
}

fn multiple_assignment_targets(context: &NodeContext, sink: &mut DiagnosticSink) {
    let node = context.node;
    if !node.is_assignment() {
        return;
    }
    let Some(left) = node.binary_parts().map(|parts| parts.left) else {
        return;
    };
    let Some(operator) = node.operator_node() else {
        return;
    };
    let operator = operator.text();
    let assignment = if matches_token::<Token![:=]>(operator) {
        ParallelAssignment::Local
    } else if matches_token::<Token![=]>(operator) {
        ParallelAssignment::Global
    } else {
        return;
    };
    if context
        .analysis
        .installation_for(node, context.source)
        .is_none()
        && !parallel_targets_are_assignable(context, left, assignment)
    {
        sink.spawn(
            &catalog::MULTIPLE_ASSIGNMENT_TARGETS,
            Finding::new(
                context.source.range_for_node(left),
                format!(
                    "{operator} parallel assignment targets must be symbols, installation \
                     targets such as `T + T` or `f ZZ`, parts such as `x#i` (with `=`), or \
                     nested lists"
                ),
            ),
        );
    }
}

/// Whether every target of a parallel assignment is one M2 assigns
/// component-wise: a symbol, a nested list of such targets, an `x <- y`
/// target (grammatical; whether it installs anything is the `<-` check's
/// concern), or an operator expression. Under `=` an operator expression is
/// an ordinary assignment to that expression (`x + 1 = v`, `M_(0,0) = v`),
/// always valid; under `:=` it must be an installation target, so a shape
/// that installs nothing or a literal operand (`x + 1`) is rejected.
fn parallel_targets_are_assignable(
    context: &NodeContext,
    targets: SyntaxNode,
    assignment: ParallelAssignment,
) -> bool {
    if !targets.is_collection_expression() {
        return true;
    }
    targets.collection_elements().all(|target| {
        target.is::<Symbol>()
            || target.has_binary_operator::<Token![<-]>()
            || (target.is_collection_expression()
                && parallel_targets_are_assignable(context, target, assignment))
            || (target.is_operator_expression()
                && (assignment == ParallelAssignment::Global
                    || is_installation_target(context, target)))
    })
}

/// Whether `target` can receive a `:=` installation: it has an installation
/// shape and no operand is a literal, which can never be a type.
fn is_installation_target(context: &NodeContext, target: SyntaxNode) -> bool {
    let position = context.source.position_for_node(target);
    context
        .analysis
        .installation_shape(target, position, context.knowledge)
        .is_some_and(|(_, operands)| operands.iter().all(|operand| !operand.is_literal()))
}

fn protected_assignment(context: &NodeContext, sink: &mut DiagnosticSink) {
    if context.primary_code {
        return;
    }
    let node = context.node;
    if !(node.has_binary_operator::<Token![=]>() || node.is::<m2_syn::nodes::ExprAssignAugmented>())
        || !node.is_assignment()
        || context
            .analysis
            .installation_for(node, context.source)
            .is_some()
    {
        return;
    }
    if let Some(parts) = node.binary_parts() {
        protected_assignment_target(context, parts.left, sink);
    }
}

/// Checks only names being rebound; an indexed or member target changes a
/// value, and an installation changes a method table rather than its binding.
fn protected_assignment_target(
    context: &NodeContext,
    target: SyntaxNode,
    sink: &mut DiagnosticSink,
) {
    let Some(target) = parenthesized_value(target) else {
        return;
    };
    if target.is_collection_expression() {
        for element in target.collection_elements() {
            protected_assignment_target(context, element, sink);
        }
        return;
    }
    if !target.is::<Symbol>() {
        return;
    }
    let name = target.text();
    let range = context.source.range_for_node(target);
    if context
        .analysis
        .assigned_binding_at(range)
        .or_else(|| context.analysis.get_binding_at(name, range.start))
        .is_some_and(|binding| binding.is_local())
    {
        return;
    }
    if context
        .knowledge
        .get_record(&ObjectName::new(name))
        .is_some_and(|record| record.protected)
    {
        sink.spawn(
            &catalog::PROTECTED_ASSIGNMENT,
            Finding::new(range, format!("Cannot assign to protected symbol `{name}`")),
        );
    }
}

/// An expression selecting a variable a ring the program writes out does
/// not have: a subscript `R_i` past its variables, or a lookup `"v"_R` of a
/// name none of them has.
fn ring_variable_selection(context: &NodeContext, sink: &mut DiagnosticSink) {
    let Some(Err(error)) =
        context
            .analysis
            .selected_ring_variable(context.node, context.source, context.knowledge)
    else {
        return;
    };
    let (rule, hint) = match &error {
        RingVariableError::IndexOutOfBounds(out_of_bounds) => (
            &catalog::RING_INDEX_OUT_OF_BOUNDS,
            match out_of_bounds.variable_count {
                1 => "this ring has 1 variable".to_string(),
                count => format!("this ring has {count} variables"),
            } + ", counting those of a polynomial coefficient ring",
        ),
        RingVariableError::NotFound { .. } => (
            &catalog::RING_VARIABLE_NOT_FOUND,
            "lookups by name find only the ring's variables, as its subscripts select them"
                .to_string(),
        ),
    };
    sink.spawn(
        rule,
        Finding::new(context.node_range(), format!("{error}; {hint}")),
    );
}

fn colon_equal_part_assignment(context: &NodeContext, sink: &mut DiagnosticSink) {
    if !context.node.has_binary_operator::<Token![:=]>() {
        return;
    }
    let Some(left) = context.node.binary_parts().map(|parts| parts.left) else {
        return;
    };
    if left.has_binary_operator::<Token![#]>() {
        sink.spawn(
            &catalog::COLON_EQUAL_PART_ASSIGNMENT,
            Finding::new(
                context.source.range_for_node(left),
                "`:=` cannot assign to parts; use `=` for part assignment",
            ),
        );
    }
}

/// A parallel assignment whose sides cannot be matched up: lists of different
/// lengths, or a single value whose type cannot supply several.
fn parallel_assignment(context: &NodeContext, sink: &mut DiagnosticSink) {
    let node = context.node;
    if !node.is_assignment()
        || !(node.has_binary_operator::<Token![=]>() || node.has_binary_operator::<Token![:=]>())
        || context
            .analysis
            .installation_for(node, context.source)
            .is_some()
    {
        return;
    }
    let (Some(targets), Some(value)) = (
        node.binary_parts().map(|parts| parts.left),
        node.binary_parts().map(|parts| parts.right),
    ) else {
        return;
    };
    match_parallel_assignment(context, targets, value, sink);
}

/// Matches a list of targets against the value assigned to it, descending into
/// nested lists of equal length.
fn match_parallel_assignment(
    context: &NodeContext,
    targets: SyntaxNode,
    value: SyntaxNode,
    sink: &mut DiagnosticSink,
) {
    if !targets.is_collection_expression() {
        return;
    }
    let target_nodes = targets.collection_elements().collect::<Vec<_>>();
    if !value.is_collection_expression() {
        parallel_assignment_value_type(context, target_nodes.len(), value, sink);
        return;
    }
    let value_nodes = value.collection_elements().collect::<Vec<_>>();
    if target_nodes.len() != value_nodes.len() {
        sink.spawn(
            &catalog::PARALLEL_ASSIGNMENT_ARITY,
            Finding::new(
                context.source.range_for_node(targets),
                format!(
                    "parallel assignment binds {} targets but the right-hand side lists {}; their lengths must match",
                    target_nodes.len(),
                    value_nodes.len()
                ),
            ),
        );
        return;
    }
    for (target, value) in target_nodes.into_iter().zip(value_nodes) {
        match_parallel_assignment(context, target, value, sink);
    }
}

/// Spawns when a single value assigned to `target_count` targets has a type
/// that cannot supply several values.
fn parallel_assignment_value_type(
    context: &NodeContext,
    target_count: usize,
    value: SyntaxNode,
    sink: &mut DiagnosticSink,
) {
    if target_count < 2 {
        return;
    }
    let mut innermost = value;
    while innermost.is_holder() {
        let Some(inner) = innermost.final_value_child() else {
            break;
        };
        innermost = inner;
    }
    if innermost.is::<Symbol>() {
        let position = context.source.position_for_node(innermost);
        let has_source_binding = context
            .analysis
            .visible_source_binding_at(innermost.text(), position, context.knowledge)
            .is_some();
        let has_indexed_value = context
            .knowledge
            .get_record(&ObjectName::new(innermost.text()))
            .is_some();
        if !has_source_binding && !has_indexed_value {
            return;
        }
    }
    let Some(value_type) =
        context
            .analysis
            .infer_expression_static_type(value, context.source, context.knowledge)
    else {
        return;
    };
    if value_type == TypeRole::Thing.object_name()
        || context
            .knowledge
            .has_type_role(&value_type, TypeRole::VisibleList)
    {
        return;
    }
    sink.spawn(
        &catalog::PARALLEL_ASSIGNMENT_TYPE,
        Finding::new(
            context.source.range_for_node(value),
            format!(
                "parallel assignment binds {target_count} targets, but the right-hand side has incompatible type `{}`",
                value_type.name()
            ),
        ),
    );
}

fn option_key_convention(context: &NodeContext, sink: &mut DiagnosticSink) {
    let node = context.node;
    if !node.has_binary_operator::<Token![=>]>() {
        return;
    }
    let Some(key) = node.binary_parts().map(|parts| parts.left) else {
        return;
    };
    if !key.is::<Symbol>() {
        return;
    }
    let key_text = key.text();
    let starts_lowercase = key_text
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_lowercase());
    if !starts_lowercase || OptionDeclaration::of_entry(node).is_none() {
        return;
    }
    sink.spawn(
        &catalog::OPTION_KEY_CONVENTION,
        Finding::new(
            context.source.range_for_node(key),
            format!("Option name `{key_text}` should be capitalized by Macaulay2 convention"),
        ),
    );
}

fn prefer_coalescence(context: &NodeContext, sink: &mut DiagnosticSink) {
    let node = context.node;
    let Some(replacement) = simplification_of(
        node,
        coalescence_rewrite(node).map(Coalescence::into_replacement),
    ) else {
        return;
    };
    sink.spawn(
        &catalog::PREFER_COALESCENCE,
        Finding::new(
            context.node_range(),
            format!("This conditional can be simplified to `{replacement}`"),
        ),
    );
}

fn ring_variable_naming(context: &NodeContext, sink: &mut DiagnosticSink) {
    let scope_idx = context
        .analysis
        .find_scope_at(context.source.position_for_node(context.node))
        .unwrap_or(0);
    let variables = context.analysis.ring_constructor_symbol_variables(
        context.node,
        context.source,
        context.knowledge,
        scope_idx,
    );
    for variable in variables {
        if let Some(message) = ring_variable_naming_message(variable.text()) {
            sink.spawn(
                &catalog::RING_VARIABLE_NAMING,
                Finding::new(context.source.range_for_node(variable), message),
            );
        }
    }
}

/// The trailing run of digits in a name, with the name before it — `x0` splits
/// into `x` and `0`. `None` when the name has no trailing digits (`xx`) or is
/// only digits, neither of which names an index the author meant to write.
fn split_trailing_index(name: &str) -> Option<(&str, &str)> {
    let index_start = name
        .char_indices()
        .rev()
        .take_while(|(_, character)| character.is_ascii_digit())
        .last()
        .map(|(byte, _)| byte)?;
    let (base, index) = name.split_at(index_start);
    (!base.is_empty()).then_some((base, index))
}

/// Why a multi-character ring variable is worth flagging: `x0` and `x1` are two
/// unrelated symbols that merely look sequential, while `x_0` and `x_1` are one
/// indexed family — subscriptable, printable as `x`, and writable as the range
/// `x_0..x_9`. Single-character names need no such treatment.
fn ring_variable_naming_message(name: &str) -> Option<String> {
    if name.chars().count() <= 1 {
        return None;
    }
    Some(match split_trailing_index(name) {
        Some((base, index)) => format!(
            "Ring variable `{name}` is a multi-character symbol, not an indexed \
             variable; use `{base}_{index}` so the generators form one indexed \
             family (`{base}_0`, `{base}_1`, ...)"
        ),
        None => format!(
            "Ring variable `{name}` is a multi-character symbol; prefer a \
             single-character name, or an indexed variable such as `{}_0`",
            name.chars()
                .next()
                .expect("a multi-character name is non-empty")
        ),
    })
}

fn install_needs_colon_equals(context: &NodeContext, sink: &mut DiagnosticSink) {
    let Some(name) = illegal_equals_install_head(context) else {
        return;
    };
    sink.spawn(
        &catalog::INSTALL_NEEDS_COLON_EQUALS,
        Finding::new(
            context.node_range(),
            format!(
                "Installing a method on `{name}` must use `:=`, not `=`: M2 rejects this \
                 (\"no method for storing values of function {name}\"). Use `:=`."
            ),
        ),
    );
}

/// The function a `f T = x -> ...` assignment tries to install a method on,
/// when `f` is a known callable.
fn illegal_equals_install_head(context: &NodeContext) -> Option<String> {
    let node = context.node;
    if !node.has_binary_operator::<Token![=]>() {
        return None;
    }
    if !node
        .binary_parts()
        .map(|parts| parts.right)?
        .is::<LambdaExpression>()
    {
        return None;
    }
    let left = node.binary_parts().map(|parts| parts.left)?;
    let position = context.source.position_for_node(node);
    let (MethodHead::Function(name), _) =
        context
            .analysis
            .installation_shape(left, position, context.knowledge)?
    else {
        return None;
    };
    (context
        .analysis
        .callable_head_kind(name.name(), position, context.knowledge)
        != CallableHeadKind::Unknown)
        .then(|| name.name().to_string())
}

/// `protect` applied to an expression, which chooses the protected symbol only
/// at run time.
fn protect_computed_symbol(context: &NodeContext, sink: &mut DiagnosticSink) {
    let node = context.node;
    if !node.is_space_application() {
        return;
    }
    let (Some(callable), Some(argument)) = (
        node.binary_parts().map(|parts| parts.left),
        node.binary_parts().map(|parts| parts.right),
    ) else {
        return;
    };
    if !callable.is::<Symbol>() || callable.text() != "protect" {
        return;
    }
    if context
        .analysis
        .binding_id_at(callable.text(), context.source.position_for_node(callable))
        .is_some()
        || argument.is::<QuoteExpression>()
        || argument.is::<Symbol>()
    {
        return;
    }
    let inferred =
        context
            .analysis
            .infer_expression_static_type(argument, context.source, context.knowledge);
    if inferred
        .as_ref()
        .is_none_or(|type_id| type_id.name() == "Symbol")
    {
        sink.spawn(
            &catalog::PROTECT_COMPUTED_SYMBOL,
            Finding::new(
                context.source.range_for_node(argument),
                "`protect` evaluates this expression to choose a Symbol at runtime; \
                 the protected symbol is not statically apparent",
            ),
        );
    }
}

fn invalid_control_transfer(context: &NodeContext, sink: &mut DiagnosticSink) {
    let node = context.node;
    if !node.is_control_transfer() {
        return;
    }
    // The enclosing function or loop is what makes a transfer legal. Inside a
    // region the grammar could not parse that structure is unknown, so a
    // missing target says the parse failed, not that the transfer is
    // misplaced — and claiming otherwise reports valid code as an error.
    if node.ancestors().any(|ancestor| ancestor.is_error()) {
        return;
    }
    let target = context
        .analysis
        .control_transfer_target(node, context.source, context.knowledge);
    if target.is_some_and(|target| target.accepts(node)) {
        return;
    }

    let message = if node.is_return_expr() {
        "`return` can only be used inside a function body"
    } else if node.is_break_expr() {
        "`break` can only be used inside a loop body or an `apply`/`scan` callback"
    } else if node.control_transfer_value().is_some()
        && matches!(
            target,
            Some(ControlTransferTarget::DoLoop(_) | ControlTransferTarget::LoopCallback { .. })
        )
    {
        "`continue` with a value requires a `list` clause"
    } else {
        "`continue` can only be used inside a `list` or `do` loop body"
    };
    let keyword = node.child(0).unwrap_or(node);
    sink.spawn(
        &catalog::INVALID_CONTROL_TRANSFER,
        Finding::new(context.source.range_for_node(keyword), message),
    );
}

fn explicit_install_required(context: &NodeContext, sink: &mut DiagnosticSink) {
    let node = context.node;
    let is_classical_left_arrow_install = if node.has_binary_operator::<Token![<-]>() {
        node.binary_parts()
            .map(|parts| parts.right)
            .filter(|right| right.has_binary_operator::<Token![:=]>())
            .and_then(|right| right.binary_parts().map(|parts| parts.right))
            .and_then(assigned_lambda)
            .is_some()
    } else {
        node.has_binary_operator::<Token![:=]>()
            && node
                .binary_parts()
                .map(|parts| parts.left)
                .and_then(parenthesized_value)
                .is_some_and(|left| left.has_binary_operator::<Token![<-]>())
            && node
                .binary_parts()
                .map(|parts| parts.right)
                .and_then(assigned_lambda)
                .is_some()
    };
    if is_classical_left_arrow_install {
        sink.spawn(
            &catalog::EXPLICIT_INSTALL_REQUIRED,
            Finding::new(
                context.node_range(),
                "Methods for `<-` must be installed with `installMethod(symbol <-, Type, function)`",
            ),
        );
    }
    let parallel_targets = node
        .is_assignment()
        .then(|| node.binary_parts().map(|parts| parts.left))
        .flatten()
        .filter(|left| left.is_collection_expression());
    for target in parallel_targets.map(left_arrow_targets).unwrap_or_default() {
        sink.spawn(
            &catalog::EXPLICIT_INSTALL_REQUIRED,
            Finding::new(
                context.source.range_for_node(target),
                "`x <- y` cannot be an assignment target: `<-` is overloaded only with \
                 `installMethod(symbol <-, Type, function)`",
            ),
        );
    }
}

/// The `x <- y` targets of a parallel assignment, at any nesting depth.
fn left_arrow_targets(node: SyntaxNode<'_>) -> Vec<SyntaxNode<'_>> {
    node.collection_elements()
        .flat_map(|target| {
            if target.has_binary_operator::<Token![<-]>() {
                vec![target]
            } else if target.is_collection_expression() {
                left_arrow_targets(target)
            } else {
                Vec::new()
            }
        })
        .collect()
}

fn condition_type(context: &NodeContext, sink: &mut DiagnosticSink) {
    let node = context.node;
    let construct = if node.is::<IfStatement>() {
        "if"
    } else if node.is::<WhileLoop>() {
        "while"
    } else {
        return;
    };
    let Some(condition) = node.control_condition() else {
        return;
    };
    let position = context.source.position_for_node(condition);
    let scope_idx = context.analysis.find_scope_at(position).unwrap_or(0);
    let checker = TypeChecker::new(context.analysis, context.knowledge);
    let actual = checker.type_of(condition, context.source, scope_idx);
    if actual.possibility_by(&TypeRole::Boolean.object_name(), |candidate, bound| {
        checker.subtype_evidence(candidate, bound, position, context.knowledge)
    }) != SubtypeEvidence::Disproven
    {
        return;
    }
    sink.spawn(
        &catalog::CONDITION_TYPE,
        Finding::new(
            context.source.range_for_node(condition),
            format!(
                "{construct} condition must have type `Boolean`, but this expression has type `{}`",
                actual.label().unwrap_or_else(|| "unknown".to_string())
            ),
        ),
    );
}

/// A method installation whose codomain inference can supply, or whose
/// annotated codomain contradicts the inferred result type.
fn install_codomain(context: &NodeContext, sink: &mut DiagnosticSink) {
    let node = context.node;
    if !node.is_assignment() {
        return;
    }
    let Some(deduction) =
        context
            .analysis
            .method_codomain_deduction(node, context.source, context.knowledge)
    else {
        return;
    };
    let codomain = &deduction.codomain;
    match (&deduction.edit, &deduction.annotated_codomain) {
        (MethodCodomainEdit::Add(_), _) => sink.spawn(
            &catalog::INSTALL_CODOMAIN_MISSING,
            Finding::new(
                deduction.diagnostic_range,
                format!(
                    "This method's lambda has the deducible codomain `{codomain}`. Add the codomain annotation."
                ),
            ),
        ),
        (MethodCodomainEdit::Replace, Some(annotated)) => sink.spawn(
            &catalog::INSTALL_CODOMAIN_MISMATCH,
            Finding::new(
                deduction.diagnostic_range,
                format!(
                    "This method's inferred result type `{codomain}` is incompatible with its annotated codomain `{annotated}`."
                ),
            ),
        ),
        (MethodCodomainEdit::Replace, None) => {}
    }
}

fn call_domain(context: &NodeContext, sink: &mut DiagnosticSink) {
    if TypeChecker::new(context.analysis, context.knowledge).call_has_disjoint_domain(
        context.node,
        context.source,
        context.knowledge,
    ) {
        sink.spawn(
            &catalog::CALL_DOMAIN_MISMATCH,
            Finding::new(
                context.node_range(),
                "No known method domain matches these argument types",
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::rewrites::{
        else_if_chain_rewrite, if_condition_rewrite, if_null_branch_rewrite,
    };
    use crate::document::DocumentSnapshot;
    use crate::object_registry::ObjectRegistry;

    #[test]
    fn ring_variables_prefer_indexed_names_over_multicharacter_symbols() {
        let builtins = ObjectRegistry::load(include_str!("../../data/m2-index.jsonl"));
        let messages = |source: &str| {
            let document = DocumentSnapshot::from_text(source.to_string(), &builtins)
                .expect("fixture should parse");
            document
                .diagnostics()
                .iter()
                .filter(|reported| reported.rule.code == catalog::RING_VARIABLE_NAMING.code)
                .map(|reported| reported.finding.message.clone())
                .collect::<Vec<_>>()
        };

        // A trailing index is what the author meant; name it as one.
        let indexed = messages("R = QQ[x0, x1, x2]\n");
        assert_eq!(
            indexed.len(),
            3,
            "each multi-character variable is reported"
        );
        assert!(
            indexed[0].contains("`x_0`") && indexed[2].contains("`x_2`"),
            "the suggestion carries the index across: {indexed:?}"
        );

        // No trailing digits, so there is no index to suggest.
        let doubled = messages("R = QQ[xx]\n");
        assert_eq!(doubled.len(), 1);
        assert!(
            doubled[0].contains("single-character") && doubled[0].contains("`x_0`"),
            "an unindexed multi-character name suggests both routes: {doubled:?}"
        );

        // Everything already idiomatic stays quiet.
        for quiet in [
            "R = QQ[x, y, z]\n",
            "R = QQ[x_0, x_1]\n",
            "R = QQ[x_0..x_3]\n",
            "R = QQ[a..d]\n",
            "R = QQ[x, Degrees => {1}]\n",
            "counter = 0\nlongName = 1\n",
        ] {
            assert!(
                messages(quiet).is_empty(),
                "{quiet:?} must not be reported: {:?}",
                messages(quiet)
            );
        }
    }

    /// A conditional nested in both branches has no flatter else-if form: moving
    /// the `then` conditional to the `else` position just swaps the two, and the
    /// swapped form proposes the original back, so the quick fix oscillated.
    #[test]
    fn conditionals_in_both_branches_have_no_simplification() {
        let text = concat!(
            "f = (label, raw) -> (\n",
            "    if label == \"Token\" then {}\n",
            "    else if not isNode raw then if #raw == 0 then {} else toList(0 .. #raw - 1)\n",
            "    else if #raw <= 1 then {}\n",
            "    else toList(1 .. #raw - 1)\n",
            "    )\n",
        );
        let builtins = ObjectRegistry::default();
        let document =
            DocumentSnapshot::from_text(text.to_string(), &builtins).expect("fixture should parse");

        let simplifications = document
            .root_node()
            .descendants()
            .filter(|node| node.is::<IfStatement>())
            .flat_map(|node| {
                [
                    if_null_branch_rewrite(node),
                    if_condition_rewrite(node),
                    else_if_chain_rewrite(node),
                ]
                .into_iter()
                .filter_map(move |rewrite| simplification_of(node, rewrite))
            })
            .collect::<Vec<_>>();
        assert!(
            simplifications.is_empty(),
            "swapping two nested conditionals is not a simplification: {simplifications:?}"
        );
    }

    /// A simplification must change the code it rewrites. A condition's span
    /// reaches the `then` keyword and so carries the separating space, which
    /// used to make `not name` compare unequal to itself and propose the
    /// untouched line back.
    #[test]
    fn simplification_is_not_reported_when_the_rewrite_changes_nothing() {
        let text = concat!(
            "f = () -> (\n",
            "    if not completed then lineNumber = lineNumber - 1;\n",
            "    g();\n",
            ");\n",
        );
        let builtins = ObjectRegistry::default();
        let document =
            DocumentSnapshot::from_text(text.to_string(), &builtins).expect("fixture should parse");

        assert!(
            document.diagnostics().is_empty(),
            "an already-simplest condition must not be reported: {:?}",
            document
                .diagnostics()
                .iter()
                .map(|reported| &reported.finding.message)
                .collect::<Vec<_>>()
        );

        let if_node = document
            .root_node()
            .descendants()
            .find(|node| node.is::<IfStatement>())
            .expect("fixture contains an if");
        for rewrite in [
            if_null_branch_rewrite(if_node),
            if_condition_rewrite(if_node),
            else_if_chain_rewrite(if_node),
            coalescence_rewrite(if_node).map(Coalescence::into_replacement),
        ] {
            assert_eq!(
                simplification_of(if_node, rewrite.clone()),
                None,
                "a rewrite equal to the original is not a simplification (got {rewrite:?})"
            );
        }
    }

    /// A control transfer is legal because of the function or loop enclosing it.
    /// When the grammar cannot parse that enclosure the structure is unknown, so
    /// reporting a scope violation turns a parser limitation into a false error
    /// on valid code. M2 v1.26.05 accepts all three shapes below and each returns
    /// `5`; the grammar currently fails on the lambda, so only the honest
    /// "Syntax error" may remain.
    #[test]
    fn control_transfers_in_unparsed_regions_report_no_scope_violation() {
        let builtins = ObjectRegistry::default();
        for source in [
            "f = () -> (\n    return -- note\n    5;\n)\n",
            "f = () -> (\n    return -- note\n    5\n)\n",
            "f = () -> (\n    return\n    5\n)\n",
            "f = () -> (\n    while true do (\n        break -- note\n        5\n    )\n)\n",
        ] {
            let document = DocumentSnapshot::from_text(source.to_string(), &builtins)
                .expect("fixture should parse");
            let scope_claims = document
                .diagnostics()
                .iter()
                .filter(|reported| reported.finding.message.contains("can only be used inside"))
                .map(|reported| reported.finding.message.clone())
                .collect::<Vec<_>>();

            assert!(
                scope_claims.is_empty(),
                "{source:?} must not be reported as a misplaced transfer: {scope_claims:?}"
            );
        }
    }

    #[test]
    fn misplaced_control_transfers_are_still_reported() {
        // The guard above must not silence transfers in code that does parse.
        let document =
            DocumentSnapshot::from_text("return 5\n".to_string(), &ObjectRegistry::default())
                .expect("fixture should parse");

        assert!(
            document
                .diagnostics()
                .iter()
                .any(|reported| reported.finding.message.contains("can only be used inside")),
            "a top-level return is still a scope violation: {:?}",
            document
                .diagnostics()
                .iter()
                .map(|reported| &reported.finding.message)
                .collect::<Vec<_>>()
        );
    }
}
