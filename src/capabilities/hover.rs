//! Hover rendering for local analysis results and indexed builtin records.

pub mod layout;

/// What follows a signature in hover whose codomain is deduced, not stated.
const DEDUCED_CODOMAIN_COMMENT: &str = "  -- deduced";

use tower_lsp::lsp_types::*;

use crate::analysis::{
    Analysis, CallStaticFacts, CodomainOrigin, FunctionInfo, MethodInstallation,
};
use crate::document::DocumentSnapshot;
use crate::meta::{BindingRole, Metadata};
use crate::node_metadata::SyntaxNode;
use crate::object_registry::ObjectName;
use crate::record_lsp::{record_hover_with_package_and_usage, signature_usage_from_facts};
use crate::record_lsp::{LspKnowledge, SignatureUsage};
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
        let mut hover = if let Some(symbol) = document.mention_symbol(&mention) {
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
            record_hover_with_package_and_usage(record, Some(&package), knowledge, None)
        } else {
            workspace_symbol_hover(name, uri, workspace)?
        };
        hover.range = Some(mention.range());
        return Some(hover);
    }

    let node = document.node_at_position_minimal(position)?;

    if !hoverable_symbol_or_operator_node(node) {
        return None;
    }

    let node_text = node.text();

    if let Some(symbol) = document.source_symbol_at(node_text, position) {
        let local_installation_signature =
            analysis.local_method_installation_signature_at(node, document);
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
    let signature_usage =
        call_signature_usage_for_hover(node, node_text, document, analysis, knowledge);
    Some(record_hover_with_package_and_usage(
        record,
        Some(&package),
        knowledge,
        signature_usage.as_ref(),
    ))
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
        Some(M2SemanticTokenType::Namespace) => "user-defined package",
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

/// The signature usage at a call site, for hover specialization: the argument
/// types at the enclosing application/operator, resolved against the loaded scope.
fn call_signature_usage_for_hover(
    node: SyntaxNode,
    node_text: &str,
    source: &(impl SourceNavigation + ?Sized),
    analysis: &crate::analysis::Analysis,
    knowledge: &(impl LspKnowledge + ?Sized),
) -> Option<SignatureUsage> {
    let parent = node.parent()?;

    let facts = if parent.is_space_application() {
        let callable = parent.binary_parts().map(|parts| parts.left)?;
        if callable.id() != node.id() {
            return None;
        }

        let argument = parent.binary_parts().map(|parts| parts.right)?;
        analysis.infer_call_static_facts(argument, source, knowledge)
    } else if parent
        .operator_node()
        .is_some_and(|operator| operator.id() == node.id())
    {
        let left = parent.binary_parts().map(|parts| parts.left)?;
        let right = parent.binary_parts().map(|parts| parts.right)?;
        CallStaticFacts {
            argument_types: vec![
                analysis.infer_expression_type(left, source, knowledge),
                analysis.infer_expression_type(right, source, knowledge),
            ],
            literal_options: Vec::new(),
        }
    } else {
        return None;
    };

    let callable = ObjectName::new(node_text);
    let signature_facts = analysis.infer_external_call_signature_facts(
        &callable,
        &facts,
        source.position_for_node(parent),
        knowledge,
    )?;
    signature_usage_from_facts(
        knowledge,
        knowledge.get_record(&callable)?,
        &signature_facts,
    )
}

/// Whether a hover over this node is meaningful: a symbol-like leaf or an
/// operator token of an expression.
fn hoverable_symbol_or_operator_node(node: SyntaxNode) -> bool {
    if node.is_symbol_like() {
        return !node.is_member_key();
    }

    node.is_operator()
}

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
    use crate::node_metadata::SyntaxParser;
    use crate::object_registry::ObjectRegistry;
    use crate::source::DocumentSource;
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
        let mut parser = SyntaxParser::new().expect("Macaulay2 parser should load");
        let analysis = analyze(parser.parse("").expect("empty fixture should parse"));
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
        let mut parser = SyntaxParser::new().expect("Macaulay2 parser should load");
        let analysis = analyze(parser.parse(text).expect("fixture should parse"));
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
        let mut parser = SyntaxParser::new().expect("Macaulay2 parser should load");
        let tree = parser.parse_tree(text, None).expect("fixture should parse");
        let root = tree.root(text);
        let analysis = analyze(root);
        let source = DocumentSource::new(text.to_string());
        let position = pos!(1, 0);
        let node = root
            .descendant_for_point_range(
                crate::source::SourcePoint::new(1, 0),
                crate::source::SourcePoint::new(1, 0),
            )
            .expect("method name node should be found");
        let symbol = analysis
            .get_binding_at("p", position)
            .expect("method symbol should be visible");
        let (method, pinned_signature) = analysis
            .local_method_installation_signature_at(node, &source)
            .expect("method installation should pin the installed signature");

        let hover = local_symbol_hover(
            "p",
            &symbol,
            &analysis,
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
        let mut parser = SyntaxParser::new().expect("Macaulay2 parser should load");
        let tree = parser.parse_tree(text, None).expect("fixture should parse");
        let root = tree.root(text);
        let analysis = analyze(root);
        let source = DocumentSource::new(text.to_string());

        for (line, expected_codomain) in [(1, "List"), (2, "Array")] {
            let point = crate::source::SourcePoint::new(line, 0);
            let node = root
                .descendant_for_point_range(point, point)
                .expect("method name node should be found");
            let (_, installation) = analysis
                .local_method_installation_signature_at(node, &source)
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
        let mut parser = SyntaxParser::new().expect("Macaulay2 parser should load");
        let tree = parser.parse_tree(text, None).expect("fixture should parse");
        let root = tree.root(text);
        let source = DocumentSource::new(text.to_string());
        let analysis = Analysis::new_with_knowledge(root, None, &source, &scoped);
        let node = root
            .descendant_for_point_range(
                crate::source::SourcePoint::new(0, 5),
                crate::source::SourcePoint::new(0, 5),
            )
            .expect("openOut node should be found");

        let usage = call_signature_usage_for_hover(node, "openOut", &source, &analysis, &scoped)
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
        let mut parser = SyntaxParser::new().expect("Macaulay2 parser should load");
        let tree = parser.parse_tree(text, None).expect("fixture should parse");
        let root = tree.root(text);
        let source = DocumentSource::new(text.to_string());
        let analysis = Analysis::new_with_knowledge(root, None, &source, &scoped);
        let node = root
            .descendant_for_point_range(
                crate::source::SourcePoint::new(2, 7),
                crate::source::SourcePoint::new(2, 7),
            )
            .expect("+ node should be found");
        assert!(
            hoverable_symbol_or_operator_node(node),
            "operator tokens should be hoverable"
        );

        let usage = call_signature_usage_for_hover(node, "+", &source, &analysis, &scoped)
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
