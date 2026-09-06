//! Diagnostic quick fixes and syntax-preserving refactors offered by the LSP.

use std::collections::HashMap;

use m2_syn::nodes::{ExprIf as IfStatement, ExprTry as TryStatement, StringLiteral, Symbol};
use m2_syn::Token;
use tower_lsp::lsp_types::Range as TextRange;
use tower_lsp::lsp_types::*;

use crate::analysis::{
    ambiguous_float_member_access_rewrite, coalescence_rewrite, else_if_chain_rewrite,
    if_condition_rewrite, if_null_branch_rewrite, redundant_control_parentheses_inner,
    try_statement_rewrite, MethodCodomainEdit,
};
use crate::diagnostic_declarations;
use crate::diagnostic_registry::{diagnostic_has_kind, DiagnosticKind};
use crate::document::DocumentSnapshot;
use crate::node_metadata::{token_spelling, M2Node};
use crate::source::SourceNavigation;
use crate::util::TextRangeExt;

struct CodeActionContext<'tree, 'request> {
    document: &'tree DocumentSnapshot,
    uri: &'request Url,
    position: Position,
    cursor: M2Node<'tree>,
    diagnostics: &'request [Diagnostic],
}

macro_rules! push_declared_action {
    ($kind:ident, $context:ident, $actions:ident) => {};
    ($kind:ident, $context:ident, $actions:ident, $action:ident) => {
        if let Some(action) = $action($context) {
            $actions.push(CodeActionOrCommand::CodeAction(action));
        }
    };
}

macro_rules! declared_code_actions {
    (diagnostics { $($phase:ident { $($kind:ident {
        code: $code:literal, name: $name:literal, severity: $severity:ident,
        check: $check:ident
        $(, action: $action:ident)? $(,)?
    }),+ $(,)? })+ } standalone_actions {
        $($standalone:ident: $standalone_action:ident),* $(,)?
    }) => {
        |context: &CodeActionContext<'_, '_>| {
            let mut actions = CodeActionResponse::new();
            $($(push_declared_action!($kind, context, actions $(, $action)?);)+)+
            $(
                if let Some(action) = $standalone_action(context) {
                    actions.push(CodeActionOrCommand::CodeAction(action));
                }
            )*
            actions
        }
    };
}

fn diagnostic_at(context: &CodeActionContext<'_, '_>, kind: DiagnosticKind) -> Option<Diagnostic> {
    context
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic_has_kind(diagnostic, kind)
                && diagnostic.range.contains_position(context.position)
        })
        .cloned()
}

fn method_codomain_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    let diagnostic = diagnostic_at(context, DiagnosticKind::InstallCodomainMissing)?;
    let mut current = Some(context.cursor);
    while let Some(node) = current {
        if node.is_assignment() {
            let knowledge = context
                .document
                .object_registry()
                .at(context.document.position_for_node(node));
            if let Some(deduction) = context.document.analysis().method_codomain_deduction(
                node,
                context.document,
                &knowledge,
            ) {
                if deduction.diagnostic_range == diagnostic.range {
                    let MethodCodomainEdit::Add(edit_range) = deduction.edit else {
                        return None;
                    };
                    return Some(
                        CodeActionSpec {
                            title: "Add codomain annotation",
                            kind: CodeActionKind::QUICKFIX,
                            is_preferred: Some(true),
                            diagnostics: Some(vec![diagnostic]),
                        }
                        .build(
                            context.uri,
                            edit_range,
                            format!("{} => ", deduction.codomain),
                        ),
                    );
                }
            }
        }
        current = node.parent();
    }
    None
}

fn colon_equal_part_assignment_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    assignment_operator_action(
        context,
        DiagnosticKind::ColonEqualPartAssignment,
        token_spelling::<Token![:=]>(),
        token_spelling::<Token![=]>(),
        "Use `=` for part assignment",
    )
}

fn install_needs_colon_equals_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    assignment_operator_action(
        context,
        DiagnosticKind::InstallNeedsColonEquals,
        token_spelling::<Token![=]>(),
        token_spelling::<Token![:=]>(),
        "Use `:=` for method installation",
    )
}

fn assignment_operator_action(
    context: &CodeActionContext<'_, '_>,
    kind: DiagnosticKind,
    current_operator: &str,
    replacement: &str,
    title: &'static str,
) -> Option<CodeAction> {
    let diagnostic = diagnostic_at(context, kind)?;
    let mut current = Some(context.cursor);
    while let Some(node) = current {
        if node.binary_operator() == Some(current_operator) {
            let operator = node.child_by_field_name("operator")?;
            return Some(
                CodeActionSpec {
                    title,
                    kind: CodeActionKind::QUICKFIX,
                    is_preferred: Some(true),
                    diagnostics: Some(vec![diagnostic]),
                }
                .build(
                    context.uri,
                    context.document.range_for_node(operator),
                    replacement.to_string(),
                ),
            );
        }
        current = node.parent();
    }
    None
}

fn option_key_convention_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    let diagnostic = diagnostic_at(context, DiagnosticKind::OptionKeyConvention)?;
    let key = enclosing_node_with_range(context, diagnostic.range, |node| node.is::<Symbol>())?;
    let mut replacement = key.text().to_string();
    replacement.get_mut(..1)?.make_ascii_uppercase();
    Some(
        CodeActionSpec {
            title: "Capitalize option key",
            kind: CodeActionKind::QUICKFIX,
            is_preferred: Some(true),
            diagnostics: Some(vec![diagnostic]),
        }
        .build(
            context.uri,
            context.document.range_for_node(key),
            replacement,
        ),
    )
}

fn redundant_control_parentheses_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    let diagnostic = diagnostic_at(context, DiagnosticKind::RedundantControlParentheses)?;
    let parentheses =
        enclosing_node_with_range(context, diagnostic.range, |node| node.is_holder())?;
    let inner = redundant_control_parentheses_inner(parentheses)?;
    let range = diagnostic.range;
    Some(
        CodeActionSpec {
            title: "Remove redundant parentheses",
            kind: CodeActionKind::QUICKFIX,
            is_preferred: Some(true),
            diagnostics: Some(vec![diagnostic]),
        }
        .build(context.uri, range, inner.text().to_string()),
    )
}

fn coalescence_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    let diagnostic = diagnostic_at(context, DiagnosticKind::PreferCoalescence)?;
    let mut current = Some(context.cursor);
    while let Some(node) = current {
        if context.document.range_for_node(node) == diagnostic.range {
            let replacement = coalescence_rewrite(node)?;
            let range = diagnostic.range;
            let title = if node.is_assignment() {
                "Use `??=` coalescing assignment"
            } else {
                "Use `??` coalescence"
            };
            return Some(
                CodeActionSpec {
                    title,
                    kind: CodeActionKind::QUICKFIX,
                    is_preferred: Some(true),
                    diagnostics: Some(vec![diagnostic]),
                }
                .build(context.uri, range, replacement),
            );
        }
        current = node.parent();
    }
    None
}

fn protect_assigned_symbol_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    let diagnostic = diagnostic_at(context, DiagnosticKind::ProtectAssignedSymbol)?;
    let symbol = enclosing_node_with_range(context, diagnostic.range, |node| node.is::<Symbol>())?;
    let start = context.document.position_for_node(symbol);
    Some(
        CodeActionSpec {
            title: "Protect the symbol itself",
            kind: CodeActionKind::QUICKFIX,
            is_preferred: Some(true),
            diagnostics: Some(vec![diagnostic]),
        }
        .build(
            context.uri,
            TextRange::new(start, start),
            "symbol ".to_string(),
        ),
    )
}

fn enclosing_node_with_range<'tree>(
    context: &CodeActionContext<'tree, '_>,
    range: TextRange,
    matches: impl Fn(M2Node<'tree>) -> bool,
) -> Option<M2Node<'tree>> {
    if let Some(symbol) = context.document.symbol_node_at_position(range.start) {
        if matches(symbol) && context.document.range_for_node(symbol) == range {
            return Some(symbol);
        }
    }
    let mut current = Some(context.cursor);
    while let Some(node) = current {
        if matches(node) && context.document.range_for_node(node) == range {
            return Some(node);
        }
        current = node.parent();
    }
    None
}

/// The code actions offered at `position`: every action from the registry
/// whose producer returns `Some`. The deepest CST node covering `position` is
/// resolved a single time here and threaded through the registry, so the
/// tree-sitter descent happens once per request instead of once per producer.
pub fn available_code_actions(
    document: &DocumentSnapshot,
    uri: &Url,
    position: Position,
    diagnostics: &[Diagnostic],
) -> Option<CodeActionResponse> {
    let cursor = document.node_at_position_minimal(position)?;
    let context = CodeActionContext {
        document,
        uri,
        position,
        cursor,
        diagnostics,
    };
    let actions = diagnostic_declarations!(declared_code_actions)(&context);
    (!actions.is_empty()).then_some(actions)
}

/// The shared shape of every action this module emits: a title, a kind, and
/// the optional LSP flags (`is_preferred`, `diagnostics`) — plus a single
/// text edit applied to `uri`. Bundling them here collapses the ~15-line
/// `WorkspaceEdit`/`CodeAction` boilerplate that every per-action producer
/// would otherwise repeat.
struct CodeActionSpec {
    title: &'static str,
    kind: CodeActionKind,
    is_preferred: Option<bool>,
    diagnostics: Option<Vec<Diagnostic>>,
}

impl CodeActionSpec {
    fn build(self, uri: &Url, range: TextRange, new_text: String) -> CodeAction {
        CodeAction {
            title: self.title.to_string(),
            kind: Some(self.kind),
            diagnostics: self.diagnostics,
            edit: Some(WorkspaceEdit {
                changes: Some(HashMap::from([(
                    uri.clone(),
                    vec![TextEdit { range, new_text }],
                )])),
                document_changes: None,
                change_annotations: None,
            }),
            command: None,
            is_preferred: self.is_preferred,
            disabled: None,
            data: None,
        }
    }
}

/// Refactor: rewrite a heavily-escaped string literal as a raw `///…///` string
/// when the value survives verbatim (no unsupported escapes, no `///` inside).
fn convert_to_raw_string_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    let string_node = context
        .cursor
        .enclosing_node(|node| node.is::<StringLiteral>())?;
    let replacement = raw_string_replacement(string_node)?;

    Some(
        CodeActionSpec {
            title: "Convert to raw string",
            kind: CodeActionKind::REFACTOR_REWRITE,
            is_preferred: None,
            diagnostics: None,
        }
        .build(
            context.uri,
            context.document.range_for_node(string_node),
            replacement,
        ),
    )
}

/// Quickfix for the ambiguous-float diagnostic (`x.3` parses as `x SPACE .3`):
/// rewrite to the member access the user almost certainly meant (`x#3`).
fn ambiguous_float_member_access_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    let diagnostic = diagnostic_at(context, DiagnosticKind::AmbiguousFloatMemberAccess)?;
    let expression = context
        .cursor
        .enclosing_node(|node| node.is_adjacent_expr() || node.is_binary_expr())?;
    let replacement = ambiguous_float_member_access_rewrite(expression)?;

    Some(
        CodeActionSpec {
            title: "Rewrite as member access",
            kind: CodeActionKind::QUICKFIX,
            is_preferred: Some(true),
            diagnostics: Some(vec![diagnostic]),
        }
        .build(
            context.uri,
            context.document.range_for_node(expression),
            replacement,
        ),
    )
}

/// Refactor: drop a redundant `else null` (or `then null`, negating the
/// condition) from an `if` statement.
fn conditional_null_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    let diagnostic = diagnostic_at(context, DiagnosticKind::SimplifiableExpression)?;
    let if_node = context
        .cursor
        .enclosing_node(|node| node.is::<IfStatement>())?;
    let replacement = if_null_branch_rewrite(if_node)?;

    Some(
        CodeActionSpec {
            title: "Simplify unnecessary null branch",
            kind: CodeActionKind::REFACTOR_REWRITE,
            is_preferred: None,
            diagnostics: Some(vec![diagnostic]),
        }
        .build(
            context.uri,
            context.document.range_for_node(if_node),
            replacement,
        ),
    )
}

/// Refactor: simplify a `try` statement — drop a redundant `then` echo or a
/// redundant `else null`.
fn simplify_try_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    let diagnostic = diagnostic_at(context, DiagnosticKind::SimplifiableExpression)?;
    let try_node = context
        .cursor
        .enclosing_node(|node| node.is::<TryStatement>())?;
    let replacement = try_statement_rewrite(try_node)?;

    Some(
        CodeActionSpec {
            title: "Simplify try",
            kind: CodeActionKind::REFACTOR_REWRITE,
            is_preferred: None,
            diagnostics: Some(vec![diagnostic]),
        }
        .build(
            context.uri,
            context.document.range_for_node(try_node),
            replacement,
        ),
    )
}

/// Refactor: push a leading `not` through a parenthesized comparison
/// (`if not (a == b) then x` → `if a != b then x`).
fn simplify_if_condition_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    let diagnostic = diagnostic_at(context, DiagnosticKind::SimplifiableExpression)?;
    let if_node = context
        .cursor
        .enclosing_node(|node| node.is::<IfStatement>())?;
    let replacement = if_condition_rewrite(if_node)?;

    Some(
        CodeActionSpec {
            title: "Simplify if condition",
            kind: CodeActionKind::REFACTOR_REWRITE,
            is_preferred: None,
            diagnostics: Some(vec![diagnostic]),
        }
        .build(
            context.uri,
            context.document.range_for_node(if_node),
            replacement,
        ),
    )
}

fn flatten_else_if_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    let diagnostic = diagnostic_at(context, DiagnosticKind::SimplifiableExpression)?;
    let mut current = Some(context.cursor);
    let mut candidate = None;
    while let Some(node) = current {
        if node.is::<IfStatement>() {
            if let Some(replacement) = else_if_chain_rewrite(node) {
                candidate = Some((node, replacement));
            }
        }
        current = node.parent();
    }
    let (node, replacement) = candidate?;
    Some(
        CodeActionSpec {
            title: "Flatten nested if into else-if chain",
            kind: CodeActionKind::REFACTOR_REWRITE,
            is_preferred: None,
            diagnostics: Some(vec![diagnostic]),
        }
        .build(
            context.uri,
            context.document.range_for_node(node),
            replacement,
        ),
    )
}

fn raw_string_replacement(string_node: M2Node<'_>) -> Option<String> {
    let content = string_node.string_literal_inner_text()?;
    let escape_count = count_string_escapes(content);
    if escape_count <= 2 {
        return None;
    }

    let unescaped = unescape_string_literal_content(content)?;
    if unescaped.contains("///") {
        return None;
    }

    Some(format!("///{unescaped}///"))
}

fn count_string_escapes(content: &str) -> usize {
    let mut chars = content.chars().peekable();
    let mut count = 0;
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek().is_some() {
            count += 1;
            chars.next();
        }
    }
    count
}

fn unescape_string_literal_content(content: &str) -> Option<String> {
    let mut result = String::new();
    let mut chars = content.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            result.push(ch);
            continue;
        }

        let escaped = chars.next()?;
        match escaped {
            '\\' => result.push('\\'),
            '"' => result.push('"'),
            'n' => result.push('\n'),
            'r' => result.push('\r'),
            't' => result.push('\t'),
            // Any escape we do not faithfully decode (octal \NNN, hex \xNN,
            // \a \b \f \v, ...) aborts the conversion. A raw string ///...///
            // is verbatim, so pushing the bare trailing character would silently
            // change the string's value (e.g. "\134" is one backslash, not "134").
            _ => return None,
        }
    }
    Some(result)
}
