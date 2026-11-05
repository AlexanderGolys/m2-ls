//! Diagnostic quick fixes and syntax-preserving refactors offered by the LSP.
//!
//! Quick fixes come from the findings themselves: a check that knows the edit
//! fixing its finding records it as a [`FindingFix`]. Rewrites come from the
//! [`RewriteSites`] of the cells around the cursor, offered as the quick fix of
//! the hint reporting them or, where the hint is absent or suppressed, as a
//! plain refactor.

use std::collections::HashMap;

use m2_syn::nodes::{Expr, ExprLiteral};
use m2_syn::visit::{self, Visit};
use tower_lsp::lsp_types::Range as TextRange;
use tower_lsp::lsp_types::*;

use crate::diagnostics::rewrites::{RewriteKind, RewriteSite, RewriteSites};
use crate::diagnostics::suppression::{DiagnosticSuppressions, SuppressionScope};
use crate::diagnostics::{catalog, DiagnosticCode, DiagnosticRule, FindingFix};
use crate::document::{DocumentSnapshot, FileProperties};
use crate::node_metadata::ParsedCell;
use crate::settings::FormattingSettings;
use crate::source::{SourceNavigation, SyntaxSource};
use crate::string_literal::LiteralSource;
use crate::syntax::CursorSyntax;
use crate::util::TextRangeExt;

use super::formatting::{format_action_edit, FormatOptions};

/// The fewest escapes that make a string literal worth writing raw.
const RAW_STRING_MIN_ESCAPES: usize = 3;

/// The delimiter of a raw string literal, on both sides.
const RAW_STRING_DELIMITER: &str = "///";

/// Document and request data shared by code-action producers.
struct CodeActionContext<'a, 'request> {
    /// Immutable document snapshot being queried.
    document: &'a DocumentSnapshot,
    /// Document location associated with the request.
    uri: &'request Url,
    /// LSP UTF-16 position at which the request is evaluated.
    position: Position,
    /// Expressions enclosing the cursor, outermost first.
    enclosing: Vec<&'a Expr>,
    /// Rewrite sites of the cells at the cursor.
    sites: Vec<RewriteSite<'a>>,
    /// Diagnostics the client reports for the document.
    diagnostics: &'request [Diagnostic],
}

/// A rewrite offered for the hint reporting it, with its action title.
fn rewrite_title(site: &RewriteSite<'_>) -> &'static str {
    match site.kind {
        RewriteKind::AmbiguousFloatMemberAccess => "Rewrite as member access",
        RewriteKind::RedundantControlParentheses => "Remove redundant parentheses",
        RewriteKind::CoalescingAssignment => "Use `??=` coalescing assignment",
        RewriteKind::Coalescence => "Use `??` coalescence",
        RewriteKind::MappingComposition => "Simplify mapped composition",
        RewriteKind::BooleanComparison => "Simplify Boolean comparison",
        RewriteKind::PartialApplicationLambda => "Replace lambda with partial application",
        RewriteKind::ForwardingLambda => "Replace forwarding lambda with function",
        RewriteKind::NullReturnGuard => "Simplify null-return guard",
        RewriteKind::NullBranch => "Simplify unnecessary null branch",
        RewriteKind::TrySimplification => "Simplify try",
        RewriteKind::IfCondition => "Simplify if condition",
        RewriteKind::ElseIfChain => "Flatten nested if into else-if chain",
    }
}

/// Whether a rewrite of `kind` is offered only as the fix of its reported
/// hint, never as a refactor of its own.
fn requires_diagnostic(kind: RewriteKind) -> bool {
    matches!(
        kind,
        RewriteKind::AmbiguousFloatMemberAccess
            | RewriteKind::CoalescingAssignment
            | RewriteKind::Coalescence
    )
}

impl<'a> CodeActionContext<'a, '_> {
    /// Builds the request context: the expressions and rewrite sites of the
    /// cells containing `position`.
    fn new<'request>(
        document: &'a DocumentSnapshot,
        uri: &'request Url,
        position: Position,
        diagnostics: &'request [Diagnostic],
    ) -> Option<CodeActionContext<'a, 'request>> {
        let byte = document.byte_for_position(position)?;
        let cells = document
            .syntax()
            .cells_intersecting(byte..byte)
            .iter()
            .filter_map(ParsedCell::syntax);
        Some(CodeActionContext {
            document,
            uri,
            position,
            enclosing: CursorSyntax::at(cells.clone(), byte, document).expressions,
            sites: RewriteSites::collect_analyzed(
                cells,
                document,
                document.analysis(),
                document.object_registry(),
            ),
            diagnostics,
        })
    }

    /// The client's diagnostic of `rule` at the cursor.
    fn diagnostic_at(&self, rule: &DiagnosticRule) -> Option<Diagnostic> {
        self.diagnostics
            .iter()
            .find(|diagnostic| {
                DiagnosticCode::from_lsp(diagnostic) == Some(rule.code)
                    && diagnostic.range.contains_position(self.position)
            })
            .cloned()
    }

    /// The client's diagnostic of `rule` exactly covering `range`.
    fn diagnostic_covering(&self, rule: &DiagnosticRule, range: TextRange) -> Option<&Diagnostic> {
        self.diagnostics.iter().find(|diagnostic| {
            DiagnosticCode::from_lsp(diagnostic) == Some(rule.code) && diagnostic.range == range
        })
    }

    /// The quick fixes the findings at the cursor record, in diagnostic order.
    fn finding_fixes(&self) -> Vec<CodeAction> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.range.contains_position(self.position))
            .filter_map(|diagnostic| {
                let fix = recorded_fix(self.document, diagnostic)?;
                Some(
                    CodeActionSpec {
                        title: fix.title.clone(),
                        kind: CodeActionKind::QUICKFIX,
                        is_preferred: Some(true),
                        diagnostics: Some(vec![diagnostic.clone()]),
                    }
                    .build(self.uri, fix.range, fix.replacement.clone()),
                )
            })
            .collect()
    }

    /// The rewrite of `site`: the preferred quick fix of the hint reported on
    /// exactly its expression, or a plain refactor where the hint is absent
    /// or suppressed.
    fn rewrite(&self, site: &RewriteSite<'_>) -> Option<CodeAction> {
        let range = self.document.range_for_syntax(site.expression)?;
        let diagnostic = self.diagnostic_covering(site.kind.rule(), range);
        if diagnostic.is_none() && requires_diagnostic(site.kind) {
            return None;
        }
        Some(
            CodeActionSpec {
                title: rewrite_title(site).into(),
                kind: if diagnostic.is_some() {
                    CodeActionKind::QUICKFIX
                } else {
                    CodeActionKind::REFACTOR_REWRITE
                },
                is_preferred: diagnostic.map(|_| true),
                diagnostics: diagnostic.map(|diagnostic| vec![diagnostic.clone()]),
            }
            .build(self.uri, range, site.replacement.clone()),
        )
    }

    /// The site of `kind` rewriting `expression`.
    fn site_of(&self, expression: &Expr, kind: RewriteKind) -> Option<&RewriteSite<'a>> {
        self.sites
            .iter()
            .find(|site| site.kind == kind && std::ptr::eq(site.expression, expression))
    }

    /// The innermost enclosing expression offering a rewrite of `kind`.
    fn innermost_site(&self, kind: RewriteKind) -> Option<&RewriteSite<'a>> {
        self.enclosing
            .iter()
            .rev()
            .find_map(|expression| self.site_of(expression, kind))
    }

    /// The rewrites offered at the cursor, each kind once:
    /// - a member access, coalescence, `try` simplification, or condition
    ///   simplification of the innermost expression offering one;
    /// - the redundant parentheses of the innermost control expression
    ///   around the cursor;
    /// - the else-if chain of the outermost conditional around the cursor.
    fn rewrites(&self) -> Vec<CodeAction> {
        let byte = self.document.byte_for_position(self.position);
        let innermost = |kind: RewriteKind, accepts: fn(&Expr) -> bool| {
            let expression = self
                .enclosing
                .iter()
                .rev()
                .find(|expression| accepts(expression))?;
            self.site_of(expression, kind)
        };
        let parentheses = self
            .sites
            .iter()
            .filter(|site| site.kind == RewriteKind::RedundantControlParentheses)
            .filter_map(|site| {
                let owner = site.owner.clone()?;
                byte.is_some_and(|byte| owner.start <= byte && byte <= owner.end)
                    .then_some((owner.len(), site))
            })
            .min_by_key(|(length, _)| *length)
            .map(|(_, site)| site);
        let member_access = self.innermost_site(RewriteKind::AmbiguousFloatMemberAccess);
        let coalescence = self.enclosing.iter().rev().find_map(|expression| {
            self.site_of(expression, RewriteKind::Coalescence)
                .or_else(|| self.site_of(expression, RewriteKind::CoalescingAssignment))
        });
        let else_if_chain = self
            .enclosing
            .iter()
            .find_map(|expression| self.site_of(expression, RewriteKind::ElseIfChain));
        let is_if = |expression: &Expr| matches!(expression, Expr::If(_));
        let is_try = |expression: &Expr| matches!(expression, Expr::Try(_));
        [
            member_access,
            coalescence,
            parentheses,
            self.innermost_site(RewriteKind::BooleanComparison),
            self.innermost_site(RewriteKind::ForwardingLambda),
            self.innermost_site(RewriteKind::PartialApplicationLambda),
            self.innermost_site(RewriteKind::MappingComposition),
            innermost(RewriteKind::NullReturnGuard, is_if),
            innermost(RewriteKind::NullBranch, is_if),
            innermost(RewriteKind::TrySimplification, is_try),
            innermost(RewriteKind::IfCondition, is_if),
            else_if_chain,
        ]
        .into_iter()
        .flatten()
        .filter_map(|site| self.rewrite(site))
        .collect()
    }

    /// Refactor: rewrite a heavily-escaped string literal as a raw
    /// `///…///` string when the value survives verbatim.
    fn raw_string(&self) -> Option<CodeAction> {
        let literal = self
            .enclosing
            .last()
            .filter(|expression| matches!(expression, Expr::Literal(ExprLiteral::String(_))))?;
        raw_string_edit(self.document, self.uri, literal)
    }
}

/// The fix the document's finding behind the client's `diagnostic` records.
fn recorded_fix<'a>(
    document: &'a DocumentSnapshot,
    diagnostic: &Diagnostic,
) -> Option<&'a FindingFix> {
    let code = DiagnosticCode::from_lsp(diagnostic)?;
    document
        .diagnostics()
        .iter()
        .find(|reported| reported.rule.code == code && reported.finding.range == diagnostic.range)?
        .finding
        .fix
        .as_ref()
}

/// The raw-string refactor of the string literal `literal`.
fn raw_string_edit(document: &DocumentSnapshot, uri: &Url, literal: &Expr) -> Option<CodeAction> {
    let replacement = raw_string_replacement(literal.literal_source(document)?)?;
    Some(
        CodeActionSpec {
            title: "Convert to raw string".into(),
            kind: CodeActionKind::REFACTOR_REWRITE,
            is_preferred: None,
            diagnostics: None,
        }
        .build(uri, document.range_for_syntax(literal)?, replacement),
    )
}

/// The code actions offered at `position`: the quick fixes the findings
/// there record, the rewrites around the cursor, then the directives and the
/// file-wide fix.
pub fn available_code_actions(
    document: &DocumentSnapshot,
    uri: &Url,
    position: Position,
    diagnostics: &[Diagnostic],
    formatting: &FormattingSettings,
) -> Option<CodeActionResponse> {
    let context = CodeActionContext::new(document, uri, position, diagnostics)?;
    let options = FormatOptions::from(formatting);
    let mut actions: CodeActionResponse = context
        .finding_fixes()
        .into_iter()
        .chain(context.raw_string())
        .chain(context.rewrites())
        .chain(primary_code_action(&context))
        .chain(suppression_actions(&context))
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
    if let Some(action) = fix_all_action(document, uri, diagnostics, &options) {
        actions.push(CodeActionOrCommand::CodeAction(action));
    }
    (!actions.is_empty()).then_some(actions)
}

/// The string literals of visited syntax.
#[derive(Default)]
struct StringLiterals<'ast>(Vec<&'ast Expr>);

impl<'ast> Visit<'ast> for StringLiterals<'ast> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if let Expr::Literal(ExprLiteral::String(_)) = expression {
            self.0.push(expression);
        }
        visit::visit_expr(self, expression);
    }
}

/// Collect the fixes and rewrites throughout the document. Prefer smaller
/// edits when proposals overlap so no edit overwrites another fix.
fn fix_all_action(
    document: &DocumentSnapshot,
    uri: &Url,
    diagnostics: &[Diagnostic],
    options: &FormatOptions,
) -> Option<CodeAction> {
    let cells = document
        .syntax()
        .cells()
        .iter()
        .filter_map(ParsedCell::syntax);
    let mut literals = StringLiterals::default();
    for cell in cells.clone() {
        literals.visit_cell(cell);
    }
    let sites = RewriteSites::collect_analyzed(
        cells,
        document,
        document.analysis(),
        document.object_registry(),
    );
    let context = CodeActionContext {
        document,
        uri,
        position: Position::default(),
        enclosing: Vec::new(),
        sites: Vec::new(),
        diagnostics,
    };
    // Only the outermost conditional of a nest flattens into one chain.
    let chain_bytes = sites
        .iter()
        .filter(|site| site.kind == RewriteKind::ElseIfChain)
        .filter_map(|site| document.syntax_bytes(site.expression))
        .collect::<Vec<_>>();
    let nested_chain = |site: &RewriteSite<'_>| {
        site.kind == RewriteKind::ElseIfChain
            && document.syntax_bytes(site.expression).is_some_and(|bytes| {
                chain_bytes.iter().any(|outer| {
                    *outer != bytes && outer.start <= bytes.start && bytes.end <= outer.end
                })
            })
    };
    let mut edits = diagnostics
        .iter()
        .filter_map(|diagnostic| recorded_fix(document, diagnostic))
        .map(|fix| TextEdit {
            range: fix.range,
            new_text: fix.replacement.clone(),
        })
        .chain(
            literals
                .0
                .into_iter()
                .filter_map(|literal| raw_string_edit(document, uri, literal))
                .chain(
                    sites
                        .iter()
                        .filter(|site| !nested_chain(site))
                        .filter_map(|site| context.rewrite(site)),
                )
                .filter_map(|action| action.edit?.changes?.remove(uri))
                .flatten(),
        )
        .collect::<Vec<_>>();
    edits.sort_by_key(|edit| {
        let bytes = document.bytes_for_range(edit.range).unwrap();
        (bytes.len(), bytes.start, edit.new_text.clone())
    });
    edits.dedup();
    let mut compatible: Vec<TextEdit> = Vec::new();
    for mut edit in edits {
        if compatible.iter().any(|accepted| {
            edit.range == accepted.range
                || (edit.range.start < accepted.range.end && accepted.range.start < edit.range.end)
        }) {
            continue;
        }
        format_action_edit(document, &mut edit, options);
        compatible.push(edit);
    }
    if compatible.is_empty() {
        return None;
    }
    compatible.sort_by_key(|edit| (edit.range.start, edit.range.end));
    Some(CodeAction {
        title: "Fix all in file".into(),
        kind: Some(CodeActionKind::SOURCE_FIX_ALL),
        edit: Some(WorkspaceEdit {
            changes: Some(HashMap::from([(uri.clone(), compatible)])),
            ..WorkspaceEdit::default()
        }),
        ..CodeAction::default()
    })
}

/// Quick fix for assigning a protected symbol in a file implementing the
/// library itself: declare the file primary code. Like the suppressions, it
/// writes a directive, so fix-all leaves it out.
fn primary_code_action(context: &CodeActionContext<'_, '_>) -> Option<CodeAction> {
    let diagnostic = context.diagnostic_at(&catalog::PROTECTED_ASSIGNMENT)?;
    let position =
        DiagnosticSuppressions::directive_position(SuppressionScope::File, diagnostic.range.start);
    Some(
        CodeActionSpec {
            title: "Analyze this file as library implementation code".into(),
            kind: CodeActionKind::QUICKFIX,
            is_preferred: None,
            diagnostics: Some(vec![diagnostic]),
        }
        .build(
            context.uri,
            TextRange::new(position, position),
            format!("{}\n", FileProperties::primary_code_directive()),
        ),
    )
}

/// Quick fixes silencing each m2-ls diagnostic at the cursor with a
/// directive comment: on its line, then in the whole file. They come last,
/// after the fixes that address the diagnostic.
fn suppression_actions(context: &CodeActionContext<'_, '_>) -> Vec<CodeAction> {
    let mut actions = Vec::new();
    for diagnostic in context
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.range.contains_position(context.position))
    {
        let Some(code) = DiagnosticCode::from_lsp(diagnostic) else {
            continue;
        };
        for (scope, place) in [
            (SuppressionScope::NextLine, "on this line"),
            (SuppressionScope::File, "in this file"),
        ] {
            let position =
                DiagnosticSuppressions::directive_position(scope, diagnostic.range.start);
            let indentation = match scope {
                SuppressionScope::NextLine => context
                    .document
                    .text()
                    .lines()
                    .nth(position.line as usize)
                    .map(|line| &line[..line.len() - line.trim_start().len()])
                    .unwrap_or_default(),
                _ => "",
            };
            let comment = DiagnosticSuppressions::directive_comment(scope, code);
            actions.push(
                CodeActionSpec {
                    title: format!("Disable {} {place}", code.as_str()),
                    kind: CodeActionKind::QUICKFIX,
                    is_preferred: None,
                    diagnostics: Some(vec![diagnostic.clone()]),
                }
                .build(
                    context.uri,
                    TextRange::new(position, position),
                    format!("{indentation}{comment}\n"),
                ),
            );
        }
    }
    actions
}

/// The shared shape of every action this module emits: a title, a kind, and
/// the optional LSP flags (`is_preferred`, `diagnostics`) — plus a single
/// text edit applied to `uri`. Bundling them here collapses the ~15-line
/// `WorkspaceEdit`/`CodeAction` boilerplate that every per-action producer
/// would otherwise repeat.
struct CodeActionSpec {
    /// User-facing title of the proposed action.
    title: String,
    /// Semantic or presentation category of this item.
    kind: CodeActionKind,
    /// Whether the client should prefer this action over other applicable fixes.
    is_preferred: Option<bool>,
    /// Findings associated with this request or collected by this pass.
    diagnostics: Option<Vec<Diagnostic>>,
}

impl CodeActionSpec {
    /// Builds a protocol code action replacing the specified range in one document.
    fn build(self, uri: &Url, range: TextRange, new_text: String) -> CodeAction {
        CodeAction {
            title: self.title,
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

/// The raw `///…///` form of a string literal's `content`, when it has
/// enough escapes to be worth it and its value survives verbatim: no escape
/// the conversion cannot decode, and no `///` inside.
fn raw_string_replacement(content: &str) -> Option<String> {
    if count_string_escapes(content) < RAW_STRING_MIN_ESCAPES {
        return None;
    }
    let unescaped = unescape_string_literal_content(content)?;
    if unescaped.contains(RAW_STRING_DELIMITER) {
        return None;
    }
    Some(format!(
        "{RAW_STRING_DELIMITER}{unescaped}{RAW_STRING_DELIMITER}"
    ))
}

/// Counts string escapes in the supplied source text.
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

/// Decodes string literal content according to Macaulay2 string-literal rules.
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
    use crate::node_metadata::SyntaxSnapshot;
    use crate::object_registry::ObjectRegistry;
    use crate::package_index::SourceResolver;
    use crate::settings::DiagnosticSettings;

    fn formatted(text: &str, settings: &FormattingSettings) -> String {
        let syntax = SyntaxSnapshot::parse(text, None);
        assert!(!syntax.has_error(), "{text}");
        format_document_with_options(&syntax, &FormatOptions::from(settings))
    }

    #[test]
    fn fix_all_collects_fixes_and_rewrites_across_the_file() {
        let source = "-- λ 😀\nx.3\ny.4\nif ready then value else null\n";
        let registry = ObjectRegistry::default();
        let document = DocumentSnapshot::from_text(source.into(), &registry).unwrap();
        let uri = Url::parse("file:///test.m2").unwrap();
        let diagnostics = visible_diagnostics(
            &document,
            &uri,
            &DiagnosticSettings::default(),
            &SourceResolver::default(),
        );
        let action = fix_all_action(&document, &uri, &diagnostics, &FormatOptions::default())
            .expect("file-wide fixes should be offered");
        assert_eq!(action.kind, Some(CodeActionKind::SOURCE_FIX_ALL));
        let changes = action.edit.unwrap().changes.unwrap();
        let edits = &changes[&uri];
        assert_eq!(edits.len(), 3);
        assert!(edits
            .windows(2)
            .all(|pair| pair[0].range.end <= pair[1].range.start));
        let mut result = source.to_string();
        for edit in edits.iter().rev() {
            result.replace_range(
                document.bytes_for_range(edit.range).unwrap(),
                &edit.new_text,
            );
        }
        assert_eq!(result, "-- λ 😀\nx#3\ny#4\nif ready then\n\tvalue\n");
        let rewritten = DocumentSnapshot::from_text(result, &registry).unwrap();
        let diagnostics = visible_diagnostics(
            &rewritten,
            &uri,
            &DiagnosticSettings::default(),
            &SourceResolver::default(),
        );
        assert!(
            fix_all_action(&rewritten, &uri, &diagnostics, &FormatOptions::default()).is_none()
        );
    }

    #[test]
    fn fix_all_does_not_include_suppression_comments_or_overlapping_edits() {
        let source = "if not (left == right) then value else null\n";
        let document =
            DocumentSnapshot::from_text(source.into(), &ObjectRegistry::default()).unwrap();
        let uri = Url::parse("file:///test.m2").unwrap();
        let diagnostics = visible_diagnostics(
            &document,
            &uri,
            &DiagnosticSettings::default(),
            &SourceResolver::default(),
        );
        let action =
            fix_all_action(&document, &uri, &diagnostics, &FormatOptions::default()).unwrap();
        let changes = action.edit.unwrap().changes.unwrap();
        let edits = &changes[&uri];
        assert!(edits
            .windows(2)
            .all(|pair| pair[0].range.end <= pair[1].range.start));
        assert!(edits.iter().all(|edit| !edit.new_text.contains("m2-ls:")));
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
        let diagnostics = visible_diagnostics(
            &document,
            &uri,
            &DiagnosticSettings::default(),
            &SourceResolver::default(),
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

    /// Where the fixture's expression, the first one of the function body on
    /// line 2, starts.
    fn rewritten_expression_start(document: &DocumentSnapshot) -> Position {
        let line = document.text().lines().nth(2).unwrap();
        Position::new(2, (line.len() - line.trim_start().len()) as u32)
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
                    let diagnostics = visible_diagnostics(
                        &document,
                        &uri,
                        &DiagnosticSettings::default(),
                        &SourceResolver::default(),
                    );
                    let expression_start = rewritten_expression_start(&document);
                    let actions = available_code_actions(
                        &document,
                        &uri,
                        expression_start,
                        &diagnostics,
                        &settings,
                    )
                    .unwrap_or_else(|| panic!("missing {title}: {source}"));
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
                    let offered_again = available_code_actions(
                        &rewritten,
                        &uri,
                        rewritten_expression_start(&rewritten),
                        &[],
                        &settings,
                    )
                    .into_iter()
                    .flatten()
                    .any(|action| {
                        matches!(action, CodeActionOrCommand::CodeAction(action)
                            if action.title == title)
                    });
                    assert!(
                        !offered_again,
                        "{title} must not apply to its own result: {source}"
                    );
                }
            }
        }
    }
}
