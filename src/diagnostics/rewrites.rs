//! Source rewrites shared by diagnostics and their quick fixes: the shorter or
//! clearer form a flagged expression has, as replacement text.

use m2_syn::nodes::{
    ElseClause, ExprFor as ForLoop, ExprIf as IfStatement, ExprLambda as LambdaExpression,
    ExprTry as TryStatement, ExprWhile as WhileLoop, FloatLiteral, Symbol, ThenClause,
    TokenParseRule, TOKEN_PARSE_RULES,
};
use m2_syn::Token;

use crate::analysis::{clause_of, clause_value, parenthesized_value, symbol_node_text};
use crate::node_metadata::{matches_token, token_spelling, M2Node};

pub fn redundant_control_parentheses_inner(node: M2Node<'_>) -> Option<M2Node<'_>> {
    if !node.is_holder() {
        return None;
    }
    let parent = node.parent()?;
    let is_field = |field| {
        parent
            .child_by_field_name(field)
            .is_some_and(|value| value.id() == node.id())
    };
    let is_control_expression = if parent.is::<IfStatement>() || parent.is::<WhileLoop>() {
        is_field("condition")
    } else if parent.is::<TryStatement>() {
        is_field("value")
    } else if parent.is_iteration_range() {
        ["iterated_collection", "range_start", "range_end"]
            .into_iter()
            .any(is_field)
    } else if parent.is::<ForLoop>() {
        is_field("filter")
    } else {
        false
    };
    is_control_expression
        .then(|| parenthesized_value(node))
        .flatten()
}

pub fn ambiguous_float_member_access_rewrite(node: M2Node<'_>) -> Option<String> {
    if !node.is_space_application() {
        return None;
    }

    let left = node.child_by_field_name("left")?;
    let right = node.child_by_field_name("right")?;
    if symbol_node_text(left).is_none()
        || !right.is::<FloatLiteral>()
        || left.end_byte() != right.start_byte()
    {
        return None;
    }

    let member_index = member_index_for_ambiguous_float_literal(right.text())?;
    Some(format!("{}#{member_index}", left.text()))
}

fn member_index_for_ambiguous_float_literal(float_text: &str) -> Option<String> {
    let fractional_part = float_text.strip_prefix('.')?;
    (!fractional_part.is_empty() && fractional_part.chars().all(|ch| ch.is_ascii_digit()))
        .then(|| fractional_part.to_string())
}

/// What a null test simplifies to with `??`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coalescence {
    /// A coalescing assignment, `x ??= fallback`.
    Assignment(String),
    /// A coalescence expression, `x ?? fallback`, possibly assigned.
    Expression(String),
}

impl Coalescence {
    /// The replacement source text.
    pub fn into_replacement(self) -> String {
        match self {
            Self::Assignment(replacement) | Self::Expression(replacement) => replacement,
        }
    }
}

/// The `??` form of a null test at `node`:
/// - `x = if x === null then v else x` becomes `x ??= v`;
/// - `if x === null then x = v`, whose own value is unused, becomes `x ??= v`;
/// - `if x === null then y = v else y = x` becomes `y = x ?? v`;
/// - `if x === null then v else x` becomes `x ?? v`;
/// - `if (try e) === null then v else e` and `try (if e === null then v else e)
///   else v` become `e ?? v`, since `??` turns errors in its left side into the
///   fallback just as the `try` does.
pub fn coalescence_rewrite(node: M2Node<'_>) -> Option<Coalescence> {
    if node.is::<TryStatement>() {
        return guarded_conditional_rewrite(node);
    }
    if node.has_binary_operator::<Token![=]>() {
        let target = node.child_by_field_name("left")?;
        let conditional = node.child_by_field_name("right")?;
        if !target.is::<Symbol>() || !conditional.is::<IfStatement>() {
            return None;
        }
        let (subject, fallback) = coalescence_parts(conditional, SubjectErrors::Propagate)?;
        return (target.text() == subject.text()).then(|| {
            Coalescence::Assignment(format!("{} ??= {}", target.text(), fallback.text()))
        });
    }
    if !node.is::<IfStatement>() {
        return None;
    }
    let rewritten_by_enclosing = node
        .parent()
        .is_some_and(|parent| coalescence_rewrite(parent).is_some())
        || enclosing_try(node).is_some_and(|guard| guarded_conditional_rewrite(guard).is_some());
    if rewritten_by_enclosing {
        return None;
    }
    coalescing_assignment_rewrite(node).or_else(|| {
        let (subject, fallback) = coalescence_parts(node, SubjectErrors::Propagate)?;
        Some(coalescence_expression(subject, fallback))
    })
}

/// `try (if e === null then v else e) else v`, whose `try` turns errors in `e`
/// into the fallback as `??` does. Its `else` must repeat the fallback, or an
/// error would yield something else.
fn guarded_conditional_rewrite(try_statement: M2Node<'_>) -> Option<Coalescence> {
    let has_other_clauses = clause_of::<ThenClause>(try_statement).is_some()
        || try_statement
            .children()
            .any(|child| child.is_except_clause());
    if has_other_clauses {
        return None;
    }
    let conditional = parenthesized_value(try_statement.child_by_field_name("value")?)?;
    if !conditional.is::<IfStatement>() {
        return None;
    }
    let (subject, fallback) = coalescence_parts(conditional, SubjectErrors::Caught)?;
    let error_fallback = clause_value(clause_of::<ElseClause>(try_statement)?)?;
    same_expression(fallback, error_fallback).then(|| coalescence_expression(subject, fallback))
}

/// The `try` whose value is `node`, seen through parentheses.
fn enclosing_try(node: M2Node<'_>) -> Option<M2Node<'_>> {
    let mut current = node.parent()?;
    while current.is_holder() {
        current = current.parent()?;
    }
    current.is::<TryStatement>().then_some(current)
}

/// `subject ?? fallback`, each side parenthesized where it would otherwise
/// regroup around `??`.
fn coalescence_expression(subject: M2Node<'_>, fallback: M2Node<'_>) -> Coalescence {
    Coalescence::Expression(format!(
        "{} ?? {}",
        coalescence_subject(subject),
        coalescence_operand(fallback)
    ))
}

/// The `??` form of an `if` that assigns a fallback when its subject is null.
fn coalescing_assignment_rewrite(if_statement: M2Node<'_>) -> Option<Coalescence> {
    let NullTest {
        subject,
        null_branch,
    } = null_test(if_statement, SubjectErrors::Propagate)?;
    let then_value = clause_value(clause_of::<ThenClause>(if_statement)?)?;
    let Some(else_value) = clause_of::<ElseClause>(if_statement).and_then(clause_value) else {
        // `if x === null then x = v` yields `null` where `x ??= v` yields `x`,
        // so the two agree only when nothing uses the `if`'s own value.
        let value_is_discarded = if_statement
            .parent()
            .is_some_and(|parent| parent.is_source_cell());
        let (target, operator, fallback) = symbol_assignment(then_value)?;
        return (null_branch == NullBranch::Then
            && value_is_discarded
            && matches_token::<Token![=]>(operator)
            && target.text() == subject.text())
        .then(|| Coalescence::Assignment(format!("{} ??= {}", subject.text(), fallback.text())));
    };
    let (fallback_branch, subject_branch) = match null_branch {
        NullBranch::Then => (then_value, else_value),
        NullBranch::Else => (else_value, then_value),
    };
    let (target, operator, fallback) = symbol_assignment(fallback_branch)?;
    let (repeated_target, repeated_operator, repeated_subject) = symbol_assignment(subject_branch)?;
    if repeated_target.text() != target.text()
        || repeated_operator != operator
        || !same_expression(repeated_subject, subject)
    {
        return None;
    }
    if matches_token::<Token![=]>(operator) && target.text() == subject.text() {
        return Some(Coalescence::Assignment(format!(
            "{} ??= {}",
            subject.text(),
            fallback.text()
        )));
    }
    Some(Coalescence::Expression(format!(
        "{} {} {} ?? {}",
        target.text(),
        operator,
        coalescence_subject(subject),
        coalescence_operand(fallback)
    )))
}

/// The target symbol, operator, and value of `x = v` or `x := v`.
fn symbol_assignment(node: M2Node<'_>) -> Option<(M2Node<'_>, &str, M2Node<'_>)> {
    let operator = node.binary_operator()?;
    if !(matches_token::<Token![=]>(operator) || matches_token::<Token![:=]>(operator)) {
        return None;
    }
    let target = node
        .child_by_field_name("left")
        .filter(|target| target.is::<Symbol>())?;
    Some((target, operator, node.child_by_field_name("right")?))
}

/// `fallback` as the right operand of `??`, parenthesized when it binds more
/// loosely than `??` and would otherwise take the coalescence as its own left
/// operand, as `x ?? y -> y` reads `(x ?? y) -> y`.
fn coalescence_operand(fallback: M2Node<'_>) -> String {
    let binds_looser = fallback.is::<LambdaExpression>()
        || operator_precedence_relative_to_coalescence(fallback)
            .is_some_and(|ordering| ordering.is_lt());
    parenthesized_if(fallback, binds_looser)
}

/// `subject` as the left operand of `??`, parenthesized unless it binds more
/// tightly: `??` groups to the right, so `a or b ?? v` reads `a or (b ?? v)`,
/// and a keyword expression such as `try e` would take `?? v` as part of `e`.
fn coalescence_subject(subject: M2Node<'_>) -> String {
    let binds_tighter = !subject.is::<LambdaExpression>()
        && !subject
            .child(0)
            .is_some_and(|first| first.is_keyword_token())
        && operator_precedence_relative_to_coalescence(subject)
            .is_none_or(|ordering| ordering.is_gt());
    parenthesized_if(subject, !binds_tighter)
}

/// How the binary operator at the top of `node` binds compared with `??`;
/// `None` when `node` is not a binary expression with a known precedence.
fn operator_precedence_relative_to_coalescence(node: M2Node<'_>) -> Option<std::cmp::Ordering> {
    let coalescence = binding_precedence(token_spelling::<Token![??]>())?;
    let operator = binding_precedence(node.binary_operator()?)?;
    Some(operator.cmp(&coalescence))
}

/// The trimmed text of `node`, in parentheses when `parenthesize` holds.
fn parenthesized_if(node: M2Node<'_>, parenthesize: bool) -> String {
    let text = node.text().trim();
    if parenthesize {
        format!("({text})")
    } else {
        text.to_string()
    }
}

/// Whether `left` and `right` spell the same expression, ignoring enclosing
/// parentheses.
fn same_expression(left: M2Node<'_>, right: M2Node<'_>) -> bool {
    parenthesized_value(left)
        .zip(parenthesized_value(right))
        .is_some_and(|(left, right)| left.text().trim() == right.text().trim())
}

/// The parsing precedence of the operator spelled `spelling`.
fn binding_precedence(spelling: &str) -> Option<u8> {
    let index = TOKEN_PARSE_RULES
        .binary_search_by_key(&spelling, |(token, _)| token)
        .ok()?;
    Some(match TOKEN_PARSE_RULES[index].1 {
        TokenParseRule::Precedence(precedence)
        | TokenParseRule::Postfix(precedence)
        | TokenParseRule::Binary(precedence, _)
        | TokenParseRule::Prefix(precedence, _)
        | TokenParseRule::PrefixBinary(precedence, _, _)
        | TokenParseRule::Unary(precedence, _) => precedence,
    })
}

/// Which branch of an `if` runs when its subject is null.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NullBranch {
    Then,
    Else,
}

/// Whether errors raised while evaluating a null test's subject are caught by
/// a `try` around the whole conditional.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubjectErrors {
    Propagate,
    Caught,
}

/// What an `if` compares with `null`, and the branch that runs when it is null.
#[derive(Debug, Clone, Copy)]
struct NullTest<'tree> {
    subject: M2Node<'tree>,
    null_branch: NullBranch,
}

/// The expression an `if` compares with `null` using `===` or `=!=`. `??`
/// treats an error in its left side like `null`, so the subject must be one
/// whose errors are handled the same way: a symbol, which never fails to
/// evaluate, an expression under a bare `try` (`(try f x) === null`), or any
/// expression when a `try` around the conditional catches its errors.
fn null_test(if_statement: M2Node<'_>, subject_errors: SubjectErrors) -> Option<NullTest<'_>> {
    let condition = if_statement.child_by_field_name("condition")?;
    let operator = condition.binary_operator()?;
    let left = condition.child_by_field_name("left")?;
    let right = condition.child_by_field_name("right")?;
    let tested = match (left.is_null_symbol(), right.is_null_symbol()) {
        (true, false) => right,
        (false, true) => left,
        _ => return None,
    };
    let null_branch = if matches_token::<Token![===]>(operator) {
        NullBranch::Then
    } else if matches_token::<Token![=!=]>(operator) {
        NullBranch::Else
    } else {
        return None;
    };
    let subject = match guarded_value(tested) {
        Some(guarded) => guarded,
        None if subject_errors == SubjectErrors::Caught || tested.is::<Symbol>() => {
            parenthesized_value(tested)?
        }
        None => return None,
    };
    Some(NullTest {
        subject,
        null_branch,
    })
}

/// The expression a bare `try e` evaluates, seen through parentheses; errors
/// in it yield `null`.
fn guarded_value(node: M2Node<'_>) -> Option<M2Node<'_>> {
    let guard = parenthesized_value(node)?;
    let is_bare_try = guard.is::<TryStatement>()
        && clause_of::<ThenClause>(guard).is_none()
        && clause_of::<ElseClause>(guard).is_none()
        && !guard.children().any(|child| child.is_except_clause());
    is_bare_try
        .then(|| guard.child_by_field_name("value"))
        .flatten()
        .and_then(parenthesized_value)
}

fn coalescence_parts(
    if_statement: M2Node<'_>,
    subject_errors: SubjectErrors,
) -> Option<(M2Node<'_>, M2Node<'_>)> {
    let NullTest {
        subject,
        null_branch,
    } = null_test(if_statement, subject_errors)?;
    let then_value = clause_value(clause_of::<ThenClause>(if_statement)?)?;
    let else_value = clause_value(clause_of::<ElseClause>(if_statement)?)?;
    let (fallback, repeated_subject) = match null_branch {
        NullBranch::Then => (then_value, else_value),
        NullBranch::Else => (else_value, then_value),
    };
    same_expression(repeated_subject, subject).then_some((subject, fallback))
}

pub fn if_null_branch_rewrite(if_node: M2Node<'_>) -> Option<String> {
    let condition = if_node.child_by_field_name("condition")?;
    let then_branch = clause_of::<ThenClause>(if_node).and_then(clause_value)?;
    let else_branch = clause_of::<ElseClause>(if_node).and_then(clause_value)?;

    if else_branch.is_null_symbol() {
        return Some(format!(
            "if {} then {}",
            condition.text().trim_end(),
            then_branch.text()
        ));
    }

    if then_branch.is_null_symbol() && !else_branch.is_null_symbol() {
        return Some(format!(
            "if {} then {}",
            negated_condition_text(condition),
            else_branch.text(),
        ));
    }

    None
}

pub fn try_statement_rewrite(try_node: M2Node<'_>) -> Option<String> {
    let condition = try_node.child_by_field_name("value")?;
    let consequence = clause_of::<ThenClause>(try_node).and_then(clause_value);
    let else_clause = clause_of::<ElseClause>(try_node);
    let condition_text = condition.text();
    let consequence_text = consequence.map(|node| node.text());

    if consequence_text == Some(condition_text) && else_clause.is_none() {
        return Some(format!("try {condition_text}"));
    }

    if let Some(alternative) = else_clause.and_then(clause_value) {
        if alternative.is_null_symbol() {
            let mut simplified = format!("try {condition_text}");
            if let Some(consequence_text) = consequence_text {
                simplified.push_str(" then ");
                simplified.push_str(consequence_text);
            }
            return Some(simplified);
        }
    }

    None
}

pub fn if_condition_rewrite(if_node: M2Node<'_>) -> Option<String> {
    let condition = if_node.child_by_field_name("condition")?;
    let simplified = simplify_condition(condition)?;
    let then_branch = clause_of::<ThenClause>(if_node).and_then(clause_value)?;
    let else_clause = clause_of::<ElseClause>(if_node);

    let mut replacement = format!("if {} then {}", simplified, then_branch.text());
    if let Some(else_clause) = else_clause {
        replacement.push(' ');
        replacement.push_str(else_clause.text());
    }
    Some(replacement)
}

pub fn else_if_chain_rewrite(if_node: M2Node<'_>) -> Option<String> {
    flatten_then_if_chain(if_node).or_else(|| flatten_parenthesized_else_if_chain(if_node))
}

/// Moves a conditional nested in the `then` branch to the `else` position by
/// negating the condition. When the `else` branch is itself a conditional, the
/// swap only trades one nested branch for the other: the result is flattenable
/// back into the original, so the quick fix would oscillate between the two
/// forms without ever clearing its diagnostic.
fn flatten_then_if_chain(if_node: M2Node<'_>) -> Option<String> {
    let condition = if_node.child_by_field_name("condition")?;
    let then_branch = clause_of::<ThenClause>(if_node).and_then(clause_value)?;
    let nested_if = unwrap_parentheses(then_branch);
    if !nested_if.is::<IfStatement>() {
        return None;
    }
    let else_branch = clause_of::<ElseClause>(if_node).and_then(clause_value)?;
    if unwrap_parentheses(else_branch).is::<IfStatement>() {
        return None;
    }
    let nested_replacement =
        else_if_chain_rewrite(nested_if).unwrap_or_else(|| nested_if.text().to_string());

    Some(format!(
        "if {} then {} else {}",
        negated_condition_text(condition),
        else_branch.text(),
        nested_replacement
    ))
}

fn flatten_parenthesized_else_if_chain(if_node: M2Node<'_>) -> Option<String> {
    let else_branch = clause_of::<ElseClause>(if_node).and_then(clause_value)?;
    let nested_if = unwrap_parentheses(else_branch);
    if !nested_if.is::<IfStatement>() {
        return None;
    }

    let nested_replacement = else_if_chain_rewrite(nested_if);
    let removes_parentheses = nested_if.id() != else_branch.id();
    if !removes_parentheses && nested_replacement.is_none() {
        return None;
    }

    let replacement = nested_replacement.unwrap_or_else(|| nested_if.text().to_string());
    let start = else_branch.start_byte() - if_node.start_byte();
    let end = else_branch.end_byte() - if_node.start_byte();
    let mut flattened = if_node.text().to_string();
    flattened.replace_range(start..end, &replacement);
    Some(flattened)
}

/// A rewrite counts as a simplification only when it actually changes the
/// expression. Proposing the original text back raises a diagnostic that its own
/// quick fix cannot clear, so the user is told to fix code that is already in its
/// simplest form and the warning survives applying the action.
pub(super) fn simplification_of(node: M2Node<'_>, rewrite: Option<String>) -> Option<String> {
    rewrite.filter(|replacement| replacement.trim() != node.text().trim())
}

fn simplify_condition(node: M2Node<'_>) -> Option<String> {
    // A condition's span runs up to the `then` keyword, so it carries the
    // separating whitespace. Comparing against the untrimmed text would make
    // every `not name` look like a change and propose itself back.
    let original = node.text().trim();
    if !node.is_prefix_expr() {
        return None;
    }
    let operator = node.child_by_field_name("operator")?;
    if !matches_token::<Token![not]>(operator.text()) {
        return None;
    }
    let child = node
        .named_children()
        .find(|child| child.id() != operator.id())?;
    let inner = unwrap_parentheses(child);
    // Only invert a comparison or cancel a second `not`. Adding grouping or
    // normalizing whitespace around an unchanged negation is not simpler.
    let inverts_comparison = inner
        .binary_operator()
        .and_then(negated_binary_operator)
        .is_some();
    let cancels_negation = inner.is_prefix_expr()
        && inner
            .child_by_field_name("operator")
            .is_some_and(|operator| matches_token::<Token![not]>(operator.text()));
    if !inverts_comparison && !cancels_negation {
        return None;
    }
    let simplified = negated_condition_text(inner);
    (simplified != original).then_some(simplified)
}

fn unwrap_parentheses(node: M2Node<'_>) -> M2Node<'_> {
    if node.is_holder() && node.child_count() == 3 {
        if let Some(inner) = node.child(1) {
            return inner;
        }
    }
    node
}

/// The negation of a condition, without the whitespace a condition's span
/// carries up to the `then` keyword; callers supply their own separators, so
/// keeping it would widen the gap before `then` on every rewrite.
fn negated_condition_text(node: M2Node<'_>) -> String {
    if node.is_prefix_expr() {
        if let Some(operator) = node.child_by_field_name("operator") {
            if matches_token::<Token![not]>(operator.text()) {
                if let Some(child) = node
                    .named_children()
                    .find(|child| child.id() != operator.id())
                {
                    return child.text().trim_end().to_string();
                }
            }
        }
    }

    if let Some(negated_operator) = node.binary_operator().and_then(negated_binary_operator) {
        if let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) {
            return format!(
                "{} {} {}",
                left.text(),
                negated_operator,
                right.text().trim_end()
            );
        }
    }

    let condition = node.text().trim_end();
    let needs_parentheses = node.is_binary_expr()
        && node.binary_operator().is_none_or(|operator| {
            binding_precedence(operator)
                .zip(binding_precedence(token_spelling::<Token![not]>()))
                .is_none_or(|(operand, negation)| operand <= negation)
        });
    if needs_parentheses {
        format!("not ({condition})")
    } else {
        format!("not {condition}")
    }
}

fn negated_binary_operator(operator: &str) -> Option<&'static str> {
    if matches_token::<Token![==]>(operator) {
        Some(token_spelling::<Token![!=]>())
    } else if matches_token::<Token![!=]>(operator) {
        Some(token_spelling::<Token![==]>())
    } else if matches_token::<Token![===]>(operator) {
        Some(token_spelling::<Token![=!=]>())
    } else if matches_token::<Token![=!=]>(operator) {
        Some(token_spelling::<Token![===]>())
    } else if matches_token::<Token![<]>(operator) {
        Some(token_spelling::<Token![>=]>())
    } else if matches_token::<Token![<=]>(operator) {
        Some(token_spelling::<Token![>]>())
    } else if matches_token::<Token![>]>(operator) {
        Some(token_spelling::<Token![<=]>())
    } else if matches_token::<Token![>=]>(operator) {
        Some(token_spelling::<Token![<]>())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_metadata::M2Parser;

    #[test]
    fn negating_a_condition_only_groups_operators_looser_than_not() {
        for (condition, negation) in [
            ("linkStyles#?style", "not linkStyles#?style"),
            ("first and second", "not (first and second)"),
            ("first or second", "not (first or second)"),
            ("isNode raw", "not isNode raw"),
        ] {
            let text = format!("if {condition} then null else value\n");
            let mut parser = M2Parser::new().unwrap();
            let root = parser.parse(&text).unwrap();
            let conditional = root
                .descendants()
                .find(|node| node.is::<IfStatement>())
                .unwrap();
            assert_eq!(
                if_null_branch_rewrite(conditional),
                Some(format!("if {negation} then value"))
            );
        }
    }

    /// The `??` form of the first `Syntax` node in `text`.
    fn coalescence_of_first<Syntax: crate::node_metadata::M2SyntaxKind>(
        text: &str,
    ) -> Option<Coalescence> {
        let mut parser = M2Parser::new().expect("parser");
        let tree = parser.parse_tree(text, None).expect("fixture should parse");
        let node = tree
            .root(text)
            .descendants()
            .find(|node| node.is::<Syntax>())
            .expect("fixture contains the node");
        coalescence_rewrite(node)
    }

    fn coalescence_of_first_if(text: &str) -> Option<Coalescence> {
        coalescence_of_first::<IfStatement>(text)
    }

    /// `??` turns an error in its left side into the fallback, as a `try`
    /// around the tested expression or the whole conditional does, so those
    /// forms coalesce too, for any expression rather than only a symbol.
    #[test]
    fn try_guarded_null_tests_coalesce_like_the_error_catching_coalescence() {
        let expression = |text: &str| Some(Coalescence::Expression(text.to_string()));

        assert_eq!(
            coalescence_of_first_if("y = if (try f x) === null then 0 else f x\n"),
            expression("f x ?? 0")
        );
        assert_eq!(
            coalescence_of_first_if("if (try x) === null then x = 1\n"),
            Some(Coalescence::Assignment("x ??= 1".to_string()))
        );
        assert_eq!(
            coalescence_of_first::<TryStatement>(
                "y = try (if f x === null then 0 else f x) else 0\n"
            ),
            expression("f x ?? 0")
        );
        assert_eq!(
            coalescence_of_first_if("y = try (if x === null then 0 else x) else 0\n"),
            None,
            "the enclosing try carries the one hint"
        );
        assert_eq!(
            coalescence_of_first_if("y = if (try (a or b)) === null then 0 else (a or b)\n"),
            expression("(a or b) ?? 0"),
            "`??` groups to the right, so an equally loose left side keeps parentheses"
        );
    }

    /// Without a `try` on the tested expression an error in it propagates,
    /// where `??` would swallow it; a `try` without the matching `else` yields
    /// `null` on error rather than the fallback; and a `try` around an
    /// assignment also swallows errors from the fallback, which `??=` lets
    /// through.
    #[test]
    fn null_tests_whose_errors_behave_differently_do_not_coalesce() {
        assert_eq!(
            coalescence_of_first_if("y = if f x === null then 0 else f x\n"),
            None
        );
        for text in [
            "y = try (if f x === null then 0 else f x)\n",
            "y = try (if f x === null then 0 else f x) else 1\n",
            "try if x === null then x = 1\n",
        ] {
            assert_eq!(coalescence_of_first::<TryStatement>(text), None, "{text:?}");
        }
    }

    #[test]
    fn an_if_that_only_assigns_a_fallback_becomes_a_coalescing_assignment() {
        let assignment = |text: &str| Some(Coalescence::Assignment(text.to_string()));

        assert_eq!(
            coalescence_of_first_if(
                "f = () -> (\n    if typeInfo === null then\n        typeInfo = class value;\n    typeInfo\n    )\n"
            ),
            assignment("typeInfo ??= class value")
        );
        assert_eq!(
            coalescence_of_first_if("if x === null then x = 1\n"),
            assignment("x ??= 1"),
            "a top-level statement's value is unused"
        );
        assert_eq!(
            coalescence_of_first_if("if null === x then x = 1 else x = x\n"),
            assignment("x ??= 1")
        );
    }

    #[test]
    fn assigning_either_the_subject_or_a_fallback_becomes_a_coalescence() {
        let expression = |text: &str| Some(Coalescence::Expression(text.to_string()));

        assert_eq!(
            coalescence_of_first_if("if c === null then d = e else d = c\n"),
            expression("d = c ?? e")
        );
        assert_eq!(
            coalescence_of_first_if("if c =!= null then d := c else d := e\n"),
            expression("d := c ?? e")
        );
        assert_eq!(
            coalescence_of_first_if("y = if x === null then a -> a else x\n"),
            expression("x ?? (a -> a)"),
            "a fallback looser than `??` keeps its own parentheses"
        );
    }

    /// `if x === null then x = v` yields `null` when `x` is set, and `x ??= v`
    /// yields `x`, so the rewrite applies only where nothing uses that value.
    #[test]
    fn coalescing_assignment_leaves_ifs_whose_value_is_used_or_that_declare() {
        for text in [
            "f = () -> (if x === null then x = 1)\n",
            "y = (if x === null then x = 1)\n",
            "if x === null then x := 1\n",
            "if x =!= null then x = 1\n",
            "if x === null then y = 1\n",
        ] {
            assert_eq!(coalescence_of_first_if(text), None, "{text:?}");
        }
    }

    #[test]
    fn ambiguous_member_access_helper_requires_dot_prefixed_float() {
        assert_eq!(
            member_index_for_ambiguous_float_literal(".3"),
            Some("3".to_string())
        );
        assert_eq!(
            member_index_for_ambiguous_float_literal(".123"),
            Some("123".to_string())
        );
        assert_eq!(member_index_for_ambiguous_float_literal("3"), None);
        assert_eq!(member_index_for_ambiguous_float_literal("."), None);
        assert_eq!(member_index_for_ambiguous_float_literal(".3e2"), None);
    }
}
