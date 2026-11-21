//! Diagnostic quick fixes and syntax-preserving refactors offered by the LSP.
//!
//! Quick fixes come from the findings themselves: a check that knows the edit
//! fixing its finding records it as a [`FindingFix`]. Rewrites come from the
//! [`RewriteSites`] of the cells around the cursor, offered as the quick fix of
//! the hint reporting them or, where the hint is absent or suppressed, as a
//! plain refactor.

use std::collections::HashMap;

use m2_syn::nodes::{Expr, Literal};
use m2_syn::visit::{self, Visit};
use tower_lsp::lsp_types::Range as TextRange;
use tower_lsp::lsp_types::*;

use crate::analysis::{Method, MethodHead};
use crate::diagnostics::rewrites::{RewriteKind, RewriteSite, RewriteSites};
use crate::diagnostics::suppression::{DiagnosticSuppressions, SuppressionScope};
use crate::diagnostics::{catalog, DiagnosticCode, DiagnosticRule, FindingFix};
use crate::document::{DocumentSnapshot, FileProperties};
use crate::node_metadata::{CallParentheses, ParsedCell};
use crate::settings::FormattingSettings;
use crate::source::{SourceNavigation, SyntaxSource};
use crate::string_literal::LiteralSource;
use crate::syntax::CursorSyntax;
use crate::util::TextRangeExt;
use crate::workspace_index::{WorkspaceDefinitionKnowledge, WorkspaceIndex};

use super::formatting::{format_action_edit, FormatOptions};
use super::inlining::{Inlining, LoadedFiles};

/// The fewest escapes that make a string literal worth writing raw.
const RAW_STRING_MIN_ESCAPES: usize = 3;

/// The delimiter of a raw string literal, on both sides.
const RAW_STRING_DELIMITER: &str = "///";

/// The source action removing the parentheses the syntax does not need.
pub const REMOVE_REDUNDANT_PARENTHESES: CodeActionKind =
    CodeActionKind::new("source.removeRedundantParentheses");

/// Declaration category selecting the applicable documentation sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
enum DocumentationItemKind {
    /// A package entry page with credits and a table of contents.
    Package,
    /// An overview of a source-defined type.
    Type,
    /// A method function overview whose installations have their own signatures.
    Method,
    /// A function with one source-defined argument list.
    Function,
    /// One installed method signature.
    Installation,
    /// A callable's optional argument.
    Option,
    /// Another source binding documented as an overview.
    Value,
}

/// Source facts and presentation fields for one SimpleDoc documentation node.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DocumentationTemplate {
    /// SimpleDoc expression identifying the documented object.
    key: String,
    /// Position of the declaration above which the node is inserted.
    start: Position,
    /// Known usage expression; absent for package and type overview pages.
    usage: Option<String>,
    /// Argument names and their known types, in call order.
    inputs: Vec<(String, Option<String>)>,
    /// Known callable result type.
    output: Option<String>,
    /// Declaration category governing which sections are appropriate.
    kind: DocumentationItemKind,
}

impl DocumentationTemplate {
    /// Creates an overview node before a declaration.
    fn new(key: String, start: Position) -> Self {
        Self {
            key,
            start,
            usage: None,
            inputs: Vec::new(),
            output: None,
            kind: DocumentationItemKind::Value,
        }
    }

    /// The call of `head` on the template's input names, as in `f(x, y)`.
    fn call_usage(&self, head: &str) -> String {
        let inputs = self.inputs.iter().map(|(name, _)| name.as_str());
        format!("{head}({})", inputs.collect::<Vec<_>>().join(", "))
    }

    /// Translates the function, optional-argument, package, and overview templates
    /// from Macaulay2Doc into SimpleDoc, retaining their section order.
    fn render(&self) -> String {
        let mut text = format!("doc ///\nNode\n  Key\n    {}\n  Headline\n    Describe this item's purpose in one short sentence.\n", self.key);
        if let Some(usage) = &self.usage {
            text.push_str(&format!("  Usage\n    {usage}\n"));
            if !self.inputs.is_empty() {
                text.push_str("  Inputs\n");
                for (name, ty) in &self.inputs {
                    let annotation = ty.as_ref().map(|ty| format!(":{ty}")).unwrap_or_default();
                    text.push_str(&format!(
                        "    {name}{annotation}\n      Describe this input and its constraints.\n"
                    ));
                }
            }
            if self.kind != DocumentationItemKind::Option {
                text.push_str("  Outputs\n");
                let annotation = self
                    .output
                    .as_ref()
                    .map(|ty| format!(":{ty}"))
                    .unwrap_or_default();
                text.push_str(&format!(
                    "    result{annotation}\n      Describe the returned value.\n"
                ));
            }
        }
        if self.usage.is_some() {
            text.push_str("  Consequences\n    Item\n      Describe side effects, or remove this section if there are none.\n");
        }
        text.push_str(
            "  Description\n    Text\n      Explain what this item does and when to use it.\n",
        );
        text.push_str("    Example\n      -- Add runnable Macaulay2 code demonstrating a typical use.\n    Text\n      Explain the example and its result, or remove this paragraph.\n");
        if self.kind == DocumentationItemKind::Package {
            text.push_str("  Acknowledgement\n    Credit support or assistance relevant to the package.\n  Contributors\n    List the package contributors.\n  References\n    List relevant publications or references.\n");
        }
        text.push_str(
            "  Caveat\n    Describe limitations or special cases, or remove this section.\n",
        );
        if self.kind == DocumentationItemKind::Package {
            text.push_str(
                "  Subnodes\n    -- Add documentation keys for the package's main entry points.\n",
            );
        }
        text.push_str("///");
        text
    }
}

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
    /// The workspace, telling which package a file loaded by another belongs to.
    workspace: Option<&'request WorkspaceIndex>,
}

/// A rewrite offered for the hint reporting it, with its action title.
fn rewrite_title(site: &RewriteSite<'_>) -> &'static str {
    match site.kind {
        RewriteKind::AmbiguousFloatMemberAccess => "Rewrite as member access",
        RewriteKind::RedundantControlParentheses => "Remove redundant parentheses",
        RewriteKind::CoalescingAssignment => "Use `??=` coalescing assignment",
        RewriteKind::Coalescence => "Use `??` coalescence",
        RewriteKind::InlinedNullBinding => "Inline null-tested local with `??`",
        RewriteKind::MappingComposition => "Simplify mapped composition",
        RewriteKind::LambdaComposition => "Replace lambda with composition",
        RewriteKind::BooleanComparison => "Simplify Boolean comparison",
        RewriteKind::PartialApplicationLambda => "Replace lambda with partial application",
        RewriteKind::SelfInitializingConstructor => "Use self-initializing constructor call",
        RewriteKind::HeaderConstructor => "Use header constructor call",
        RewriteKind::WrapperConstructor => "Use wrapper constructor call",
        RewriteKind::NullReturnGuard => "Simplify null-return guard",
        RewriteKind::NullBranch => "Simplify unnecessary null branch",
        RewriteKind::TrySimplification => "Simplify try",
        RewriteKind::CoalescedTry => "Remove `try` redundant under `??`",
        RewriteKind::AugmentedAssignment => "Use augmented assignment",
        RewriteKind::IfCondition => "Simplify if condition",
        RewriteKind::ElseIfChain => "Flatten nested if into else-if chain",
    }
}

/// Whether `site` is offered only as the fix of its reported hint, never as a
/// refactor of its own: a rewrite catching errors needs the user's opt-in.
fn requires_diagnostic(site: &RewriteSite<'_>) -> bool {
    site.catches_errors
        || matches!(
            site.kind,
            RewriteKind::AmbiguousFloatMemberAccess
                | RewriteKind::CoalescingAssignment
                | RewriteKind::Coalescence
                | RewriteKind::InlinedNullBinding
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
        workspace: Option<&'request WorkspaceIndex>,
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
            workspace,
        })
    }

    /// Whether the package owning `position` exports the global `name`, so
    /// that documentation of it has a reader.
    fn is_exported(&self, name: &str, position: Position) -> bool {
        let exports = self.document.packages().exports_at(position);
        exports
            .iter()
            .any(|export| export.definition.name() == name)
            || self
                .workspace
                .is_some_and(|workspace| workspace.exports_definition(self.uri, position, name))
    }

    /// Whether `name` is a global of the workspace's own code rather than of
    /// Macaulay2 or a library package.
    fn is_source_global(&self, name: &str) -> bool {
        self.document
            .analysis()
            .binding_states()
            .any(|binding| binding.name.name() == name)
            || self
                .workspace
                .is_some_and(|workspace| workspace.is_defined(name, self.uri))
    }

    /// Whether the documentation of `installation` has a reader: its method
    /// is exported or, for a method the package does not own, such as an
    /// operator or `net`, one of the types it is installed on is.
    fn is_public_installation(&self, method: &Method, position: Position) -> bool {
        let domain_exported = method
            .domain
            .iter()
            .any(|ty| self.is_exported(ty.name(), position));
        match &method.head {
            MethodHead::Function(name) => {
                self.is_exported(name.name(), position)
                    || domain_exported && !self.is_source_global(name.name())
            }
            MethodHead::Operator(_) => domain_exported,
        }
    }

    /// Documentation placeholders for the package declared on the cursor's
    /// line and for the exported declarations enclosing the cursor and their
    /// options; documentation of anything else would have no reader.
    fn documentation_placeholders(&self) -> Vec<CodeAction> {
        let mut targets = Vec::new();
        for installation in self.document.analysis().installations() {
            let method = &installation.method;
            if !installation.span.contains_position(self.position)
                || !self.is_public_installation(method, installation.span.start)
            {
                continue;
            }
            let head = match &method.head {
                MethodHead::Function(name) => name.name().to_string(),
                MethodHead::Operator(operator) => {
                    format!("symbol {}", operator.token.name())
                }
            };
            let mut parts = vec![head];
            parts.extend(method.domain.iter().map(|name| name.name().to_string()));
            let mut template = DocumentationTemplate::new(
                format!("({})", parts.join(", ")),
                installation.span.start,
            );
            let names = self.documentation_parameters(installation.span);
            template.inputs = method
                .domain
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    (
                        names
                            .get(index)
                            .cloned()
                            .unwrap_or_else(|| format!("arg{}", index + 1)),
                        Some(ty.name().to_string()),
                    )
                })
                .collect();
            template.usage = Some(template.call_usage(method.head.name().name()));
            template.kind = DocumentationItemKind::Installation;
            template.output = method.codomain.as_ref().map(|ty| ty.name().to_string());
            targets.push(template);
        }
        if targets.is_empty() {
            for binding in self.document.analysis().binding_states() {
                if !binding
                    .state
                    .definition_range
                    .contains_position(self.position)
                    || self.document.declaration_code(binding).is_none()
                    || !self.is_exported(binding.name.name(), binding.state.definition_range.start)
                {
                    continue;
                }
                let start = binding.state.definition_range.start;
                let name = binding.name.name();
                let mut template = DocumentationTemplate::new(name.to_string(), start);
                if binding.state.presentation_kind == SymbolKind::CLASS {
                    template.kind = DocumentationItemKind::Type;
                }
                if let Some(function) = self.document.analysis().function_for_binding(binding) {
                    template.kind = if function.is_method_function() {
                        DocumentationItemKind::Method
                    } else {
                        DocumentationItemKind::Function
                    };
                    template.inputs = self
                        .documentation_parameters(binding.state.definition_range)
                        .into_iter()
                        .map(|name| (name, None))
                        .collect();
                    template.usage = Some(template.call_usage(name));
                    template.output = function
                        .typical_value
                        .as_ref()
                        .map(|ty| ty.name().to_string());
                    for option in function.declared_options.iter().flatten() {
                        let mut option_template = DocumentationTemplate::new(
                            format!("[{name}, {}]", option.name.name()),
                            start,
                        );
                        option_template.kind = DocumentationItemKind::Option;
                        option_template.usage =
                            Some(format!("{name}(..., {} => value)", option.name.name()));
                        option_template.inputs.push((
                            "value".into(),
                            option
                                .accepted_types
                                .first()
                                .map(|ty| ty.name().to_string()),
                        ));
                        targets.push(option_template);
                    }
                }
                if template.kind == DocumentationItemKind::Method {
                    template.usage = None;
                    template.inputs.clear();
                    template.output = None;
                }
                targets.push(template);
            }
        }
        for package in self.document.packages().declarations() {
            if package.section.start.line == self.position.line {
                let mut template = DocumentationTemplate::new(
                    package.name.name().to_string(),
                    package.section.start,
                );
                template.kind = DocumentationItemKind::Package;
                targets.push(template);
            }
        }
        targets.sort_by(|left, right| {
            (
                &left.key,
                left.start,
                left.kind != DocumentationItemKind::Package,
            )
                .cmp(&(
                    &right.key,
                    right.start,
                    right.kind != DocumentationItemKind::Package,
                ))
        });
        targets.dedup_by(|left, right| left.key == right.key && left.start == right.start);
        targets
            .into_iter()
            .filter_map(|template| {
                let start = template.start;
                let key = &template.key;
                let line = self.document.text().lines().nth(start.line as usize)?;
                let indent: String = line
                    .chars()
                    .take_while(|character| matches!(character, ' ' | '\t'))
                    .collect();
                // A line insertion must not split an earlier statement on the same line.
                let prefix = self.document.byte_for_position(start)?;
                let line_start = self
                    .document
                    .byte_for_position(Position::new(start.line, 0))?;
                if !self.document.text()[line_start..prefix].trim().is_empty() {
                    return None;
                }
                let text = template.render();
                let newline = if self.document.text().contains("\r\n") {
                    "\r\n"
                } else {
                    "\n"
                };
                let replacement = text
                    .lines()
                    .map(|line| format!("{indent}{line}{newline}"))
                    .collect::<String>();
                let position = Position::new(start.line, 0);
                Some(
                    CodeActionSpec {
                        title: format!("Generate SimpleDoc documentation for {key}"),
                        kind: CodeActionKind::REFACTOR_REWRITE,
                        is_preferred: None,
                        diagnostics: None,
                    }
                    .build(
                        self.uri,
                        TextRange::new(position, position),
                        replacement,
                    ),
                )
            })
            .collect()
    }

    /// Parameter names retained by typed source analysis in declaration order.
    fn documentation_parameters(&self, range: TextRange) -> Vec<String> {
        self.document
            .analysis()
            .bindings()
            .filter(|binding| {
                binding.role == crate::meta::BindingRole::Parameter
                    && range.contains_position(binding.range.start)
            })
            .map(|binding| binding.name.name().to_string())
            .collect()
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
        let range = match &site.extent {
            Some(extent) => self.document.range_for_bytes(extent.clone()),
            None => self.document.range_for_syntax(site.expression)?,
        };
        let diagnostic = self.diagnostic_covering(site.rule(), range);
        if diagnostic.is_none() && requires_diagnostic(site) {
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
                .or_else(|| self.site_of(expression, RewriteKind::InlinedNullBinding))
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
            self.innermost_site(RewriteKind::LambdaComposition),
            self.innermost_site(RewriteKind::PartialApplicationLambda),
            self.innermost_site(RewriteKind::SelfInitializingConstructor),
            self.innermost_site(RewriteKind::HeaderConstructor),
            self.innermost_site(RewriteKind::WrapperConstructor),
            self.innermost_site(RewriteKind::MappingComposition),
            innermost(RewriteKind::NullReturnGuard, is_if),
            innermost(RewriteKind::NullBranch, is_if),
            innermost(RewriteKind::TrySimplification, is_try),
            self.innermost_site(RewriteKind::CoalescedTry),
            self.innermost_site(RewriteKind::AugmentedAssignment),
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
            .filter(|expression| matches!(expression, Expr::Literal(Literal::String(_))))?;
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
/// there record, the rewrites and inlines around the cursor, then the
/// directives and the file-wide fix. An inlined load keeps the file's text
/// as written, unformatted.
pub fn available_code_actions(
    document: &DocumentSnapshot,
    uri: &Url,
    position: Position,
    diagnostics: &[Diagnostic],
    formatting: &FormattingSettings,
    workspace: Option<&WorkspaceIndex>,
    files: &LoadedFiles<'_>,
) -> Option<CodeActionResponse> {
    let context = CodeActionContext::new(document, uri, position, diagnostics, workspace)?;
    let options = FormatOptions::from(formatting);
    let inlining = Inlining::new(document, uri, position);
    let mut actions: CodeActionResponse = context
        .finding_fixes()
        .into_iter()
        .chain(context.documentation_placeholders())
        .chain(context.raw_string())
        .chain(context.rewrites())
        .chain(inlining.function_actions())
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
        .chain(inlining.load_action(files))
        .map(CodeActionOrCommand::CodeAction)
        .collect();
    if let Some(action) = fix_all_action(document, uri, diagnostics, &options) {
        actions.push(CodeActionOrCommand::CodeAction(action));
    }
    if formatting.redundant_parentheses.offers_action() {
        actions.extend(
            redundant_parentheses_action(document, uri, formatting.call_parentheses)
                .map(CodeActionOrCommand::CodeAction),
        );
    }
    (!actions.is_empty()).then_some(actions)
}

/// Removes the document's redundant parentheses, replacing each cell holding
/// some. The edits are not reformatted: only the parentheses and the
/// whitespace inside them go.
fn redundant_parentheses_action(
    document: &DocumentSnapshot,
    uri: &Url,
    calls: CallParentheses,
) -> Option<CodeAction> {
    let edits = document
        .syntax()
        .redundant_parentheses(calls)
        .into_iter()
        .map(|(bytes, new_text)| TextEdit {
            range: document.range_for_bytes(bytes),
            new_text,
        })
        .collect::<Vec<_>>();
    (!edits.is_empty()).then(|| CodeAction {
        title: "Remove redundant parentheses in file".into(),
        kind: Some(REMOVE_REDUNDANT_PARENTHESES),
        edit: Some(WorkspaceEdit {
            changes: Some(HashMap::from([(uri.clone(), edits)])),
            ..WorkspaceEdit::default()
        }),
        ..CodeAction::default()
    })
}

/// The string literals of visited syntax.
#[derive(Default)]
struct StringLiterals<'ast>(Vec<&'ast Expr>);

impl<'ast> Visit<'ast> for StringLiterals<'ast> {
    fn visit_expr(&mut self, expression: &'ast Expr) {
        if let Expr::Literal(Literal::String(_)) = expression {
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
        workspace: None,
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
    use crate::capabilities::formatting::{
        format_document_with_options, ControlFlowLayout, RedundantParentheses,
    };
    use crate::diagnostics::visible_diagnostics;
    use crate::node_metadata::SyntaxSnapshot;
    use crate::object_registry::ObjectRegistry;
    use crate::package_index::SourceResolver;
    use crate::settings::{Indent, ServerSettings};

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
            &ServerSettings::defaults().diagnostics.clone(),
            &SourceResolver::default(),
            None,
        );
        let action = fix_all_action(
            &document,
            &uri,
            &diagnostics,
            &FormatOptions::from(&ServerSettings::defaults().formatting),
        )
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
            &ServerSettings::defaults().diagnostics.clone(),
            &SourceResolver::default(),
            None,
        );
        assert!(fix_all_action(
            &rewritten,
            &uri,
            &diagnostics,
            &FormatOptions::from(&ServerSettings::defaults().formatting)
        )
        .is_none());
    }

    /// `redundantParentheses` decides whether the source action is offered,
    /// which replaces only the cells holding such parentheses, and whether
    /// formatting removes them too; `callParentheses` whether call arguments
    /// count.
    #[test]
    fn redundant_parentheses_follow_their_setting() {
        let source = "x = (a*b) + c\ny = f(x)\n";
        let document =
            DocumentSnapshot::from_text(source.into(), &ObjectRegistry::default()).unwrap();
        let uri = Url::parse("file:///test.m2").unwrap();
        let with = |redundant_parentheses, call_parentheses| FormattingSettings {
            redundant_parentheses,
            call_parentheses,
            ..ServerSettings::defaults().formatting.clone()
        };
        let edits = |settings: &FormattingSettings| {
            available_code_actions(
                &document,
                &uri,
                Position::default(),
                &[],
                settings,
                None,
                &LoadedFiles::NONE,
            )
            .into_iter()
            .flatten()
            .find_map(|action| match action {
                CodeActionOrCommand::CodeAction(action)
                    if action.kind == Some(REMOVE_REDUNDANT_PARENTHESES) =>
                {
                    action.edit?.changes?.remove(&uri)
                }
                _ => None,
            })
        };
        let first_cell = TextEdit {
            range: document.range_for_bytes(0..14),
            new_text: "x = a*b + c\n".into(),
        };
        let second_cell = TextEdit {
            range: document.range_for_bytes(14..23),
            new_text: "y = f x\n".into(),
        };

        assert_eq!(
            edits(&with(RedundantParentheses::Action, CallParentheses::Remove)),
            Some(vec![first_cell.clone(), second_cell])
        );
        assert_eq!(
            edits(&with(RedundantParentheses::Action, CallParentheses::Keep)),
            Some(vec![first_cell])
        );
        assert_eq!(
            edits(&with(RedundantParentheses::Off, CallParentheses::Remove)),
            None
        );
        assert_eq!(
            formatted(
                source,
                &with(RedundantParentheses::Action, CallParentheses::Remove)
            ),
            source
        );
        assert_eq!(
            formatted(
                source,
                &with(RedundantParentheses::Format, CallParentheses::Remove)
            ),
            "x = a*b + c\ny = f x\n"
        );
        assert_eq!(
            formatted(
                source,
                &with(RedundantParentheses::Format, CallParentheses::Keep)
            ),
            "x = a*b + c\ny = f(x)\n"
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
            &ServerSettings::defaults().diagnostics.clone(),
            &SourceResolver::default(),
            None,
        );
        let action = fix_all_action(
            &document,
            &uri,
            &diagnostics,
            &FormatOptions::from(&ServerSettings::defaults().formatting),
        )
        .unwrap();
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
        let mut diagnostics = visible_diagnostics(
            &document,
            &uri,
            &ServerSettings::defaults().diagnostics.clone(),
            &SourceResolver::default(),
            None,
        );
        diagnostics.retain(|diagnostic| {
            diagnostic.code
                != Some(NumberOrString::String(
                    catalog::UNINTRODUCED_SYMBOL.code.as_str().to_owned(),
                ))
        });
        assert!(available_code_actions(
            &document,
            &uri,
            Position::new(0, 7),
            &diagnostics,
            &ServerSettings::defaults().formatting.clone(),
            None,
            &LoadedFiles::NONE,
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
            ServerSettings::defaults().formatting.clone(),
            FormattingSettings {
                indent: Indent::Spaces(2),
                soft_line_width: Some(40),
                control_flow_layout: ControlFlowLayout::Compact,
                ..ServerSettings::defaults().formatting.clone()
            },
            FormattingSettings {
                indent: Indent::Tab,
                control_flow_layout: ControlFlowLayout::Multiline,
                ..ServerSettings::defaults().formatting.clone()
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
                        &ServerSettings::defaults().diagnostics.clone(),
                        &SourceResolver::default(),
                        None,
                    );
                    let expression_start = rewritten_expression_start(&document);
                    let actions = available_code_actions(
                        &document,
                        &uri,
                        expression_start,
                        &diagnostics,
                        &settings,
                        None,
                        &LoadedFiles::NONE,
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
                        None,
                        &LoadedFiles::NONE,
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
