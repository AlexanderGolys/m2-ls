//! Hover rendering for local analysis results and indexed builtin records.

pub mod layout;

/// When set, a hover over a builtin object ends with everything the index
/// and the call-site analysis know, for diagnosing wrong types.
const HOVER_DEBUG_ENV: &str = "M2_LS_HOVER_DEBUG";

/// What follows a signature in hover whose codomain is deduced, not stated.
const DEDUCED_CODOMAIN_COMMENT: &str = "  -- deduced";

use m2_syn::nodes::{Expr, ExprOp, ExprOpBin};
use m2_syn::{Span, Spanned};
use tower_lsp::lsp_types::*;

use crate::analysis::{
    Analysis, CallSignatureFacts, CallStaticFacts, CodomainOrigin, FunctionInfo, InfixSyntax,
    MethodInstallation,
};
use crate::document::{DocumentSnapshot, SymbolAt};
use crate::meta::{BindingRole, Metadata};
use crate::object_registry::ObjectName;
use crate::record_lsp::LspKnowledge;
use crate::record_lsp::{
    call_debug_lines, record_debug_lines, record_hover_with_package_and_usage,
    signature_usage_from_facts,
};
use crate::semantic_token::M2SemanticTokenType;
use crate::source::SourceNavigation;
use crate::workspace_index::{WorkspaceDefinitionKnowledge, WorkspaceDocumentationKnowledge};
use layout::{HoverLayout, SectionContent};

/// The hover at `position` of the document `uri`: a local symbol renders its
/// binding info and local method signatures; a builtin/package object renders
/// its record from the partition that owns it (with call-context signature
/// specialization); a name of another file of the workspace renders its role
/// there. Each shows the documentation page of the name, wherever in the
/// workspace it is written.
pub fn hover_response<W>(
    document: &DocumentSnapshot,
    uri: &Url,
    position: Position,
    knowledge: &(impl LspKnowledge + ?Sized),
    workspace: &W,
) -> Option<Hover>
where
    W: WorkspaceDefinitionKnowledge + WorkspaceDocumentationKnowledge + ?Sized,
{
    let text = document.text();
    let analysis = document.analysis();

    if let Some(mention) = document.symbol_mention_at(position) {
        let name = mention.name(text);
        let mut hover = if let Some(symbol) = document
            .mention_symbol(&mention)
            .filter(|_| !catalog_defines(document, name, knowledge))
        {
            let documentation = document
                .documentation_for_binding(symbol)
                .map(|documentation| documentation.hover_markdown().to_string())
                .or_else(|| workspace.documentation(name));
            local_symbol_hover(
                name,
                &symbol,
                analysis,
                document.callable_at_position(position),
                None,
                document.declaration_code(symbol),
                documentation,
            )
        } else if let Some((package, record)) =
            knowledge.get_record_with_package(&ObjectName(name.to_string()))
        {
            if record.callable().is_some()
                && document.object_registry().is_source_object(&record.id)
            {
                workspace_symbol_hover(name, uri, workspace)?
            } else {
                record_hover_with_package_and_usage(record, Some(&package), knowledge, None)
            }
        } else {
            workspace_symbol_hover(name, uri, workspace)?
        };
        hover.range = Some(mention.range());
        return Some(hover);
    }

    let hovered = HoveredSyntax::at(document, position)?;
    let node_text = hovered.text(document)?;

    if let Some(symbol) = document
        .source_symbol_at(node_text, position)
        .filter(|_| !catalog_defines(document, node_text, knowledge))
    {
        let local_installation_signature =
            document
                .installing_assignment_at(position)
                .and_then(|assignment| {
                    analysis.local_method_installation_signature(assignment, document)
                });
        let local_method = local_installation_signature
            .map(|(method, _)| method)
            .or_else(|| document.callable_at_position(position));
        let pinned_signature = local_installation_signature.map(|(_, signature)| signature);
        let documentation = pinned_signature
            .and_then(|installation| document.documentation_for_installation(installation.id))
            .or_else(|| document.documentation_for_binding(symbol))
            .map(|documentation| documentation.hover_markdown().to_string())
            .or_else(|| workspace.documentation(node_text));
        let code = pinned_signature
            .and_then(|installation| document.installation_code(installation))
            .or_else(|| document.declaration_code(symbol));
        return Some(local_symbol_hover(
            node_text,
            &symbol,
            analysis,
            local_method,
            pinned_signature,
            code,
            documentation,
        ));
    }

    // Render from the partition that owns the record so an imported package
    // object shows its own documentation/signatures, not only a Core lookup.
    let Some((package, record)) =
        knowledge.get_record_with_package(&ObjectName(node_text.to_string()))
    else {
        return workspace_symbol_hover(node_text, uri, workspace);
    };
    if record.callable().is_some() && document.object_registry().is_source_object(&record.id) {
        return workspace_symbol_hover(node_text, uri, workspace);
    }
    let evidence = call_evidence_for_hover(&hovered, node_text, document, analysis, knowledge);
    let signature_usage = evidence.as_ref().and_then(|(_, signature_facts)| {
        signature_usage_from_facts(knowledge, record, signature_facts)
    });
    let mut hover = record_hover_with_package_and_usage(
        record,
        Some(&package),
        knowledge,
        signature_usage.as_ref(),
    );
    if std::env::var_os(HOVER_DEBUG_ENV).is_some() {
        let mut lines = record_debug_lines(record, knowledge);
        if let Some((facts, signature_facts)) = &evidence {
            lines.push("call:".to_string());
            lines.extend(call_debug_lines(
                knowledge,
                &facts.argument_types,
                signature_facts,
            ));
        }
        if let HoverContents::Markup(content) = &mut hover.contents {
            content.value = format!(
                "```text\n{}\n```\n\n---\n\n{}",
                lines.join("\n"),
                content.value
            );
        }
    }
    Some(hover)
}

/// Whether `name` is an object of the catalog that the document, as library
/// implementation code, defines itself: hover describes the catalog's object
/// rather than the document's assignment of it.
fn catalog_defines(
    document: &DocumentSnapshot,
    name: &str,
    knowledge: &(impl LspKnowledge + ?Sized),
) -> bool {
    document.properties().primary_code
        && knowledge
            .get_record_with_package(&ObjectName::new(name))
            .is_some_and(|(_, record)| !document.object_registry().is_source_object(&record.id))
}

/// What the cursor rests on for hover, with the expression around it.
enum HoveredSyntax<'a> {
    /// A symbol reading or binding a name.
    Symbol(SymbolAt<'a>),
    /// The operator of a binary operation.
    BinaryOperator {
        /// The operation it combines operands with.
        operation: &'a Expr,
        /// Where the operator is written.
        span: Span,
    },
    /// The operator of a prefix or postfix operation.
    UnaryOperator(Span),
}

impl<'a> HoveredSyntax<'a> {
    /// The hoverable syntax at `position`: a symbol that is no member key,
    /// or an operator token.
    fn at(document: &'a DocumentSnapshot, position: Position) -> Option<Self> {
        if let Some(symbol) = document.reference_symbol_at(position) {
            return Some(Self::Symbol(symbol));
        }
        let byte = document.byte_for_position(position)?;
        let covers = |span: &Span| {
            document
                .syntax_bytes(span)
                .is_some_and(|bytes| bytes.contains(&byte))
        };
        let innermost = *document.enclosing_expressions(position).last()?;
        match innermost {
            Expr::Op(ExprOp::Prefix(prefix)) => prefix
                .operator
                .try_span()
                .filter(covers)
                .map(Self::UnaryOperator),
            Expr::Op(ExprOp::Postfix(postfix)) => postfix
                .operator
                .try_span()
                .filter(covers)
                .map(Self::UnaryOperator),
            _ => {
                let span = innermost.infix_operands()?.operator_span.filter(covers)?;
                Some(Self::BinaryOperator {
                    operation: innermost,
                    span,
                })
            }
        }
    }

    /// The hovered source text.
    fn text<'source>(&self, document: &'source DocumentSnapshot) -> Option<&'source str> {
        match self {
            Self::Symbol(at) => document.text_for_syntax(at.symbol),
            Self::BinaryOperator { span, .. } | Self::UnaryOperator(span) => {
                document.text_for_syntax(span).map(str::trim)
            }
        }
    }
}

/// The hover for a symbol resolved by the local analysis: the `code`
/// declaring it, its inferred type and role, the methods of a method
/// function — those besides the hovered installation `pinned_signature` —
/// and its `documentation`.
fn local_symbol_hover(
    name: &str,
    symbol: &(impl Metadata + ?Sized),
    analysis: &Analysis,
    method: Option<&FunctionInfo>,
    pinned_signature: Option<&MethodInstallation>,
    code: Option<&str>,
    documentation: Option<String>,
) -> Hover {
    let meta = symbol.meta();
    let role = match meta.symbol_kind {
        Some(SymbolKind::FUNCTION) if method.is_some_and(FunctionInfo::is_method_function) => {
            "user-defined method function"
        }
        Some(SymbolKind::FUNCTION) => "user-defined function",
        Some(SymbolKind::VARIABLE) if meta.binding_role == Some(BindingRole::Parameter) => {
            "function parameter"
        }
        Some(SymbolKind::VARIABLE) => "user-defined binding",
        _ => "user-defined symbol",
    };
    let mut layout = HoverLayout::new(code.unwrap_or(name));
    if let Some(type_name) = meta.type_label.as_deref() {
        layout = layout.with_fact(format!("`{type_name}`"));
    }
    layout = layout.with_fact(role);
    if let Some(method) = method {
        if let Some(codomain) = method
            .typical_value
            .as_ref()
            .filter(|_| method.installations.is_empty())
        {
            layout = layout.with_fact(format!("returns `{}`", codomain.name()));
        }
        let signatures = analysis
            .effective_installations(method)
            .filter(|installation| {
                pinned_signature.is_none_or(|pinned| {
                    installation.method.domain != pinned.method.domain
                        || installation.method.codomain != pinned.method.codomain
                })
            })
            .map(|installation| local_method_signature_code(name, method, installation));
        layout = layout.with_section(
            if pinned_signature.is_some() {
                "Other methods"
            } else {
                "Methods"
            },
            SectionContent::Code,
            signatures,
        );
    }
    layout.with_documentation(documentation).into_hover()
}

/// The hover for `name` as another file of the workspace defines or documents
/// it, asked from the document `uri`: the code defining it, its role, and its
/// documentation page.
fn workspace_symbol_hover<W>(name: &str, uri: &Url, workspace: &W) -> Option<Hover>
where
    W: WorkspaceDefinitionKnowledge + WorkspaceDocumentationKnowledge + ?Sized,
{
    let token_type = workspace
        .semantic_token(name, uri)
        .map(|token| token.token_type);
    let documentation = workspace.documentation(name);
    if token_type.is_none() && documentation.is_none() {
        return None;
    }
    let role = match token_type {
        Some(M2SemanticTokenType::Function) => "user-defined function",
        Some(M2SemanticTokenType::Method) => "user-defined method function",
        Some(M2SemanticTokenType::Type) => "user-defined type",
        Some(M2SemanticTokenType::Variable) => "user-defined binding",
        Some(M2SemanticTokenType::Package) => "user-defined package",
        _ => "user-defined symbol",
    };
    let code = workspace
        .declaration_code(name, uri)
        .unwrap_or_else(|| name.to_string());
    Some(
        HoverLayout::new(code)
            .with_fact(role)
            .with_documentation(documentation)
            .into_hover(),
    )
}

/// The call site's argument facts and how the installed signatures sort by
/// them, for hover specialization: the argument types at the enclosing
/// application/operator, resolved against the loaded scope.
fn call_evidence_for_hover(
    hovered: &HoveredSyntax<'_>,
    node_text: &str,
    document: &DocumentSnapshot,
    analysis: &Analysis,
    knowledge: &(impl LspKnowledge + ?Sized),
) -> Option<(CallStaticFacts, CallSignatureFacts)> {
    let (facts, context): (_, &dyn Spanned) = match *hovered {
        HoveredSyntax::Symbol(SymbolAt {
            expression: Some(symbol),
            parent: Some(call @ Expr::Op(ExprOp::Bin(ExprOpBin::Adj(application)))),
            ..
        }) if std::ptr::eq(application.left_operand.as_ref(), symbol) => (
            analysis.infer_call_static_facts(application, document, knowledge),
            call,
        ),
        HoveredSyntax::BinaryOperator { operation, .. } => {
            let operands = operation.infix_operands()?;
            (
                CallStaticFacts {
                    argument_types: vec![
                        analysis.infer_expression_type(operands.left, document, knowledge),
                        analysis.infer_expression_type(operands.right, document, knowledge),
                    ],
                    literal_options: Vec::new(),
                },
                operation,
            )
        }
        _ => return None,
    };

    let callable = ObjectName::new(node_text);
    let signature_facts = analysis.infer_external_call_signature_facts(
        &callable,
        &facts,
        document.position_for_syntax(context)?,
        knowledge,
    )?;
    Some((facts, signature_facts))
}

/// Whether a hover over this node is meaningful: a symbol-like leaf or an
/// operator token of an expression.
/// The method `installation` of the method function `name` as the Macaulay2
/// code of a call, `f(ZZ) -> ZZ`; a codomain only deduced says so.
fn local_method_signature_code(
    name: &str,
    method: &FunctionInfo,
    installation: &MethodInstallation,
) -> String {
    let signature = &installation.method;
    let domain = signature
        .domain
        .iter()
        .map(ObjectName::name)
        .collect::<Vec<_>>()
        .join(", ");
    let codomain = signature
        .codomain
        .as_ref()
        .or(method.typical_value.as_ref())
        .map(|codomain| format!(" -> {}", codomain.name()))
        .unwrap_or_default();
    let origin = match installation.codomain_origin {
        Some(CodomainOrigin::Deduction) => DEDUCED_CODOMAIN_COMMENT,
        _ => "",
    };
    format!("{name}({domain}){codomain}{origin}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::{BindingRole, Meta};
    use crate::object_registry::ObjectRegistry;
    use crate::record_lsp::SignatureUsage;
    use crate::test_support::analyze;
    use crate::workspace_index::WorkspaceIndex;
    use tower_lsp::lsp_types::{HoverContents, Range as TextRange, SymbolKind};

    /// The hover at `position` of `document`, alone in its workspace.
    fn hover_at(
        document: &DocumentSnapshot,
        position: Position,
        knowledge: &(impl LspKnowledge + ?Sized),
    ) -> Option<Hover> {
        let uri = Url::parse("file:///hover.m2").expect("the test URI parses");
        hover_response(
            document,
            &uri,
            position,
            knowledge,
            &WorkspaceIndex::default(),
        )
    }

    /// The key of `output.value` is the symbol `value`, not the builtin
    /// function `value` nor the parameter of that name, so it has no hover.
    /// The signature usage the call evidence at `hovered` gives `name`.
    fn usage_at(
        hovered: &HoveredSyntax<'_>,
        name: &str,
        document: &DocumentSnapshot,
        knowledge: &ObjectRegistry,
    ) -> Option<SignatureUsage> {
        let (_, signature_facts) =
            call_evidence_for_hover(hovered, name, document, document.analysis(), knowledge)?;
        signature_usage_from_facts(
            knowledge,
            knowledge.get_record(&ObjectName::new(name))?,
            &signature_facts,
        )
    }

    #[test]
    fn member_keys_have_no_hover() {
        let builtins = ObjectRegistry::load(crate::catalog::test_corpus());
        let document =
            DocumentSnapshot::from_text("f = value -> output.value\n".to_string(), &builtins)
                .expect("fixture should parse");

        assert!(hover_at(&document, pos!(0, 20), &builtins).is_none());
        assert!(hover_at(&document, pos!(0, 4), &builtins).is_some());
    }

    #[test]
    fn local_hover_includes_known_static_type() {
        let analysis = analyze("");
        let symbol = Meta {
            symbol_kind: Some(SymbolKind::VARIABLE),
            binding_role: Some(BindingRole::Ordinary),
            type_label: Some("Package".to_string()),
        };

        let hover = local_symbol_hover("Doc", &symbol, &analysis, None, None, None, None);
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("local hover should use markdown");
        };

        assert!(
            markup
                .value
                .starts_with("```macaulay2\nDoc\n```\n`Package` · user-defined binding"),
            "local hover should state the known static type under the code: {}",
            markup.value
        );
        assert!(!markup.value.contains("Defined at"));
    }

    #[test]
    fn local_hover_includes_method_signatures() {
        let text = "p = method(TypicalValue => List)\np(ZZ, ZZ) := (i, j) -> {i, j}\n";
        let analysis = analyze(text);
        let symbol = analysis
            .get_binding_at("p", pos!(1, 0))
            .expect("method symbol should be visible");
        let method = analysis
            .function_at("p", pos!(1, 0))
            .expect("method should be registered");

        let hover = local_symbol_hover("p", &symbol, &analysis, Some(method), None, None, None);
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("local hover should use markdown");
        };

        assert!(markup.value.contains("user-defined method function"));
        assert!(markup
            .value
            .contains("**Methods**\n```macaulay2\np(ZZ, ZZ) -> List\n```"));
    }

    #[test]
    fn local_method_installation_hover_pins_installed_signature() {
        let text = "p = method(TypicalValue => List)\np(ZZ, ZZ) := (i, j) -> {i, j}\np(CC, CC) := Array => (i, j) -> [i, j]\n";
        let document = DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
            .expect("fixture should parse");
        let analysis = document.analysis();
        let position = pos!(1, 0);
        let symbol = analysis
            .get_binding_at("p", position)
            .expect("method symbol should be visible");
        let (method, pinned_signature) = document
            .installing_assignment_at(position)
            .and_then(|assignment| {
                analysis.local_method_installation_signature(assignment, &document)
            })
            .expect("method installation should pin the installed signature");

        let hover = local_symbol_hover(
            "p",
            &symbol,
            analysis,
            Some(method),
            Some(pinned_signature),
            None,
            None,
        );
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("local hover should use markdown");
        };

        assert!(markup
            .value
            .contains("**Other methods**\n```macaulay2\np(CC, CC) -> Array\n```"));
        assert!(!markup.value.contains("p(ZZ, ZZ) -> List"));
    }

    #[test]
    fn repeated_domain_installations_pin_their_own_source_fact() {
        let text = concat!(
            "p = method()\n",
            "p ZZ := List => x -> x\n",
            "p ZZ := Array => x -> x\n",
        );
        let document = DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
            .expect("fixture should parse");
        let analysis = document.analysis();

        for (line, expected_codomain) in [(1, "List"), (2, "Array")] {
            let (_, installation) = document
                .installing_assignment_at(pos!(line, 0))
                .and_then(|assignment| {
                    analysis.local_method_installation_signature(assignment, &document)
                })
                .expect("the source occurrence should resolve its installation identity");
            assert_eq!(
                installation.method.codomain.as_ref().map(ObjectName::name),
                Some(expected_codomain)
            );
        }
    }

    #[test]
    fn hover_shows_imported_package_documentation() {
        // Hovering an object from an imported package renders that package's own
        // documentation (resolved from the owning partition, not just Core).
        let text = "needsPackage \"JSON\"\ntoJSON\n";
        let document = DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
            .expect("fixture should parse");
        let index = ObjectRegistry::load(crate::catalog::test_corpus());
        let scoped = index.with_source_imports(text);
        let hover = hover_at(&document, pos!(1, 0), &scoped)
            .expect("hover over an imported package object");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("imported-package hover should use markdown");
        };
        assert!(
            markup.value.contains("package `JSON`"),
            "got: {}",
            markup.value
        );
        assert!(
            markup.value.contains("Encode Macaulay2 things as JSON"),
            "expected the JSON package doc body, got: {}",
            markup.value
        );
    }

    #[test]
    fn hover_resolves_local_wikilink_mentions() {
        let text = "-- use [[x]]\nx := 1\n";
        let document = DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
            .expect("fixture should parse");
        let hover = hover_at(&document, pos!(0, 9), &ObjectRegistry::default())
            .expect("local documentation reference should have a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("local hover should use markdown");
        };

        assert!(
            markup.value.starts_with("```macaulay2\nx := 1\n```"),
            "{}",
            markup.value
        );
        assert!(markup.value.contains("user-defined binding"));
        assert_eq!(hover.range, Some(TextRange::new(pos!(0, 9), pos!(0, 10))));
    }

    #[test]
    fn hover_resolves_indexed_wikilink_mentions() {
        let text = "-- use [[ideal]]\nx := 1\n";
        let document = DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
            .expect("fixture should parse");
        let index = ObjectRegistry::load(crate::catalog::test_corpus());
        let scoped = index.with_source_imports(text);
        let hover = hover_at(&document, pos!(0, 9), &scoped)
            .expect("indexed documentation reference should have a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("indexed hover should use markdown");
        };

        assert!(
            markup.value.starts_with("```macaulay2\nideal\n```"),
            "got: {}",
            markup.value
        );
        assert_eq!(hover.range, Some(TextRange::new(pos!(0, 9), pos!(0, 14))));
    }

    #[test]
    fn local_hover_includes_attached_markdown_documentation() {
        let text = "-- Computes **the answer** with [[ideal]].\nanswer := 42\nanswer\n";
        let document = DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
            .expect("fixture should parse");
        let hover = hover_at(&document, pos!(2, 2), &ObjectRegistry::default())
            .expect("documented binding should have a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("local hover should use markdown");
        };

        assert!(markup
            .value
            .contains("Computes **the answer** with `ideal`."));
    }

    #[test]
    fn hover_call_context_specializes_builtin_method_signatures() {
        let text = "F := openOut \"test.oldvalues\"\n";
        let index = ObjectRegistry::load(crate::catalog::test_corpus());
        let scoped = index.with_source_imports("");
        let document =
            DocumentSnapshot::from_text(text.to_string(), &scoped).expect("fixture should parse");
        let hovered =
            HoveredSyntax::at(&document, pos!(0, 5)).expect("openOut should be hoverable");

        let usage = usage_at(&hovered, "openOut", &document, &scoped)
            .expect("openOut hover should resolve usage signatures");
        let signature = usage
            .pinned
            .expect("openOut hover should pin the String installation");

        assert_eq!(
            signature
                .signature
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["openOut", "String"]
        );
        assert_eq!(
            signature
                .output_types
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["File"]
        );
    }

    #[test]
    fn hover_call_context_specializes_operator_method_signatures() {
        let text = "x := 1\ny := 2\nz := x + y\n";
        let index = ObjectRegistry::load(crate::catalog::test_corpus());
        let scoped = index.with_source_imports("");
        let document =
            DocumentSnapshot::from_text(text.to_string(), &scoped).expect("fixture should parse");
        let hovered =
            HoveredSyntax::at(&document, pos!(2, 7)).expect("operator tokens should be hoverable");

        let usage = usage_at(&hovered, "+", &document, &scoped)
            .expect("+ hover should resolve usage signatures");
        let signature = usage
            .pinned
            .expect("+ hover should pin the ZZ, ZZ installation");

        assert_eq!(
            signature
                .signature
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["+", "ZZ", "ZZ"]
        );
        assert_eq!(
            signature
                .output_types
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["ZZ"]
        );
    }
}
