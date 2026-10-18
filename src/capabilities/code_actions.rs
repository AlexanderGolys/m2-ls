//! Diagnostic quick fixes and syntax-preserving refactors offered by the LSP.

use std::collections::HashMap;

use m2_syn::nodes::{ExprIf as IfStatement, ExprTry as TryStatement, StringLiteral, Symbol};
use m2_syn::Token;
use tower_lsp::lsp_types::Range as TextRange;
use tower_lsp::lsp_types::*;

use crate::analysis::MethodCodomainEdit;
use crate::diagnostics::rewrites::{
    ambiguous_float_member_access_rewrite, coalescence_rewrite, else_if_chain_rewrite,
    if_condition_rewrite, if_null_branch_rewrite, redundant_control_parentheses_inner,
    try_statement_rewrite, Coalescence,
};
use crate::diagnostics::{catalog, DiagnosticCode, DiagnosticRule};
use crate::document::DocumentSnapshot;
use crate::node_metadata::{token_spelling, M2Node};
use crate::settings::FormattingSettings;
use crate::source::SourceNavigation;
use crate::util::TextRangeExt;

use super::formatting::{format_action_edit, FormatOptions};

struct CodeActionContext<'tree, 'request> {
    document: &'tree DocumentSnapshot,
    uri: &'request Url,
    position: Position,
    cursor: M2Node<'tree>,
    diagnostics: &'request [Diagnostic],
}

/// Builds one code action for the request, or `None` when it does not apply
/// at the cursor.
type CodeActionProducer = fn(&CodeActionContext<'_, '_>) -> Option<CodeAction>;

/// Every code action m2-ls offers, in the order clients list them: quick fixes
/// for a diagnostic, ordered by diagnostic code, then refactors that apply
/// without one.
const CODE_ACTION_PRODUCERS: &[CodeActionProducer] = &[
    ambiguous_float_member_access_action,
    colon_equal_part_assignment_action,
    option_key_convention_action,
    redundant_control_parentheses_action,
    coalescence_action,
    install_needs_colon_equals_action,
    method_codomain_action,
    convert_to_raw_string_action,
    conditional_null_action,
    simplify_try_action,
    simplify_if_condition_action,
    flatten_else_if_action,
];

fn diagnostic_at(context: &CodeActionContext<'_, '_>, rule: &DiagnosticRule) -> Option<Diagnostic> {
    context
        .diagnostics
        .iter()
        .find(|diagnostic| {
            DiagnosticCode::from_lsp(diagnostic) == Some(rule.code)
                && diagnostic.range.contains_position(context.position)
        })
        .cloned()
}

fn method_codomain_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    let diagnostic = diagnostic_at(context, &catalog::INSTALL_CODOMAIN_MISSING)?;
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
        &catalog::COLON_EQUAL_PART_ASSIGNMENT,
        token_spelling::<Token![:=]>(),
        token_spelling::<Token![=]>(),
        "Use `=` for part assignment",
    )
}

fn install_needs_colon_equals_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    assignment_operator_action(
        context,
        &catalog::INSTALL_NEEDS_COLON_EQUALS,
        token_spelling::<Token![=]>(),
        token_spelling::<Token![:=]>(),
        "Use `:=` for method installation",
    )
}

fn assignment_operator_action(
    context: &CodeActionContext<'_, '_>,
    rule: &DiagnosticRule,
    current_operator: &str,
    replacement: &str,
    title: &'static str,
) -> Option<CodeAction> {
    let diagnostic = diagnostic_at(context, rule)?;
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
    let diagnostic = diagnostic_at(context, &catalog::OPTION_KEY_CONVENTION)?;
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
    let diagnostic = diagnostic_at(context, &catalog::REDUNDANT_CONTROL_PARENTHESES)?;
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
    let diagnostic = diagnostic_at(context, &catalog::PREFER_COALESCENCE)?;
    let mut current = Some(context.cursor);
    while let Some(node) = current {
        if context.document.range_for_node(node) == diagnostic.range {
            let (title, replacement) = match coalescence_rewrite(node)? {
                Coalescence::Assignment(replacement) => {
                    ("Use `??=` coalescing assignment", replacement)
                }
                Coalescence::Expression(replacement) => ("Use `??` coalescence", replacement),
            };
            let range = diagnostic.range;
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

fn enclosing_node_with_range<'tree>(
    context: &CodeActionContext<'tree, '_>,
    range: TextRange,
    matches: impl Fn(M2Node<'tree>) -> bool,
) -> Option<M2Node<'tree>> {
    if let Some(symbol) = context.document.reference_node_at_position(range.start) {
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

/// The code actions offered at `position`: every producer in
/// [`CODE_ACTION_PRODUCERS`] that applies there. The deepest CST node covering
/// `position` is resolved once here and shared by all producers.
pub fn available_code_actions(
    document: &DocumentSnapshot,
    uri: &Url,
    position: Position,
    diagnostics: &[Diagnostic],
    formatting: &FormattingSettings,
) -> Option<CodeActionResponse> {
    let cursor = document.node_at_position_minimal(position)?;
    let context = CodeActionContext {
        document,
        uri,
        position,
        cursor,
        diagnostics,
    };
    let options = FormatOptions::from(formatting);
    let actions: CodeActionResponse = CODE_ACTION_PRODUCERS
        .iter()
        .filter_map(|produce| produce(&context))
        .map(|mut action| {
            if let Some(changes) = action.edit.as_mut().and_then(|edit| edit.changes.as_mut()) {
                for edit in changes.values_mut().flatten() {
                    format_action_edit(document, edit, &options);
                }
            }
            action
        })
        .map(CodeActionOrCommand::CodeAction)
        .collect();
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
    let diagnostic = diagnostic_at(context, &catalog::AMBIGUOUS_FLOAT_MEMBER_ACCESS)?;
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
    let diagnostic = diagnostic_at(context, &catalog::SIMPLIFIABLE_EXPRESSION)?;
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
    let diagnostic = diagnostic_at(context, &catalog::SIMPLIFIABLE_EXPRESSION)?;
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
    let diagnostic = diagnostic_at(context, &catalog::SIMPLIFIABLE_EXPRESSION)?;
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
    let diagnostic = diagnostic_at(context, &catalog::SIMPLIFIABLE_EXPRESSION)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::formatting::{format_document_with_options, ControlFlowLayout};
    use crate::diagnostics::visible_diagnostics;
    use crate::node_metadata::M2Parser;
    use crate::object_registry::ObjectRegistry;
    use crate::settings::DiagnosticSettings;

    fn formatted(text: &str, settings: &FormattingSettings) -> String {
        let mut parser = M2Parser::new().unwrap();
        let root = parser.parse(text).unwrap();
        assert!(!root.has_error(), "{text}");
        format_document_with_options(root, &FormatOptions::from(settings))
    }

    #[test]
    fn negated_lookup_has_no_simplification_or_parentheses_action() {
        let source = concat!(
            "if not linkStyles#?style then\n",
            "        error(\"MarkdownDocs: unknown LinkStyle \", toString style);\n",
        );
        let document =
            DocumentSnapshot::from_text(source.into(), &ObjectRegistry::default()).unwrap();
        let uri = Url::parse("file:///test.m2").unwrap();
        let diagnostics = visible_diagnostics(&document, &uri, &DiagnosticSettings::default());
        assert!(
            !diagnostics.iter().any(|diagnostic| {
                matches!(DiagnosticCode::from_lsp(diagnostic), Some(code) if
                code == catalog::SIMPLIFIABLE_EXPRESSION.code ||
                code == catalog::REDUNDANT_CONTROL_PARENTHESES.code)
            }),
            "{diagnostics:?}"
        );
        assert!(available_code_actions(
            &document,
            &uri,
            Position::new(0, 7),
            &diagnostics,
            &FormattingSettings::default(),
        )
        .is_none());
    }

    #[test]
    fn rewritten_expressions_are_formatted_in_context() {
        let cases = [
            (
                "if not (left == right) then error(\"unknown LinkStyle\", toString style)",
                "Simplify if condition",
            ),
            (
                "if ready then value else null",
                "Simplify unnecessary null branch",
            ),
            (
                "if linkStyles#?style then null else error(\"unknown LinkStyle\", toString style)",
                "Simplify unnecessary null branch",
            ),
            (
                "if first and second then null else value",
                "Simplify unnecessary null branch",
            ),
            (
                "if first then one else (if second then two else three)",
                "Flatten nested if into else-if chain",
            ),
            ("try value then value", "Simplify try"),
            ("try value then result else null", "Simplify try"),
            ("if (condition) then value", "Remove redundant parentheses"),
            (
                "if candidate =!= null then candidate else fallback",
                "Use `??` coalescence",
            ),
        ];
        let uri = Url::parse("file:///test.m2").unwrap();
        for settings in [
            FormattingSettings::default(),
            FormattingSettings {
                indent: Some(2),
                soft_line_width: Some(40),
                control_flow_layout: ControlFlowLayout::Compact,
                ..FormattingSettings::default()
            },
            FormattingSettings {
                use_tabs: Some(true),
                control_flow_layout: ControlFlowLayout::Multiline,
                ..FormattingSettings::default()
            },
        ] {
            for (expression, title) in cases {
                for newline in ["\n", "\r\n"] {
                    let source = formatted(
                        &format!("-- λ 😀\nf = () -> (\n{expression};\n42\n)\n")
                            .replace('\n', newline),
                        &settings,
                    );
                    let document =
                        DocumentSnapshot::from_text(source.clone(), &ObjectRegistry::default())
                            .unwrap();
                    let diagnostics =
                        visible_diagnostics(&document, &uri, &DiagnosticSettings::default());
                    let diagnostic = diagnostics
                        .iter()
                        .find(|diagnostic| {
                            matches!(DiagnosticCode::from_lsp(diagnostic), Some(code) if
                            code == catalog::SIMPLIFIABLE_EXPRESSION.code ||
                            code == catalog::REDUNDANT_CONTROL_PARENTHESES.code ||
                            code == catalog::PREFER_COALESCENCE.code)
                        })
                        .unwrap_or_else(|| panic!("missing hint: {source}"));
                    let actions = available_code_actions(
                        &document,
                        &uri,
                        diagnostic.range.start,
                        &diagnostics,
                        &settings,
                    )
                    .unwrap();
                    let action = actions
                        .into_iter()
                        .find_map(|action| match action {
                            CodeActionOrCommand::CodeAction(action) if action.title == title => {
                                Some(action)
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| panic!("missing {title}: {source}"));
                    let changes = action.edit.unwrap().changes.unwrap();
                    let mut result = source.clone();
                    for edit in changes[&uri].iter().rev() {
                        result.replace_range(
                            document.bytes_for_range(edit.range).unwrap(),
                            &edit.new_text,
                        );
                    }
                    assert_eq!(result, formatted(&result, &settings), "{title}: {source}");
                    assert!(result.starts_with(&format!("-- λ 😀{newline}")));
                    let rewritten =
                        DocumentSnapshot::from_text(result, &ObjectRegistry::default()).unwrap();
                    assert!(
                        !rewritten.diagnostics().iter().any(|diagnostic| {
                            diagnostic.rule.code == catalog::SIMPLIFIABLE_EXPRESSION.code
                                || diagnostic.rule.code
                                    == catalog::REDUNDANT_CONTROL_PARENTHESES.code
                        }),
                        "{title} must clear the hint without another simplification: {source}"
                    );
                }
            }
        }
    }
}
