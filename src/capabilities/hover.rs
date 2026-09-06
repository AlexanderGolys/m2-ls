//! Hover rendering for local analysis results and indexed builtin records.

use tower_lsp::lsp_types::*;

use crate::analysis::{Analysis, CallStaticFacts, FunctionInfo, Method, MethodInstallation};
use crate::document::DocumentSnapshot;
use crate::meta::{BindingRole, Metadata};
use crate::node_metadata::M2Node;
use crate::object_registry::ObjectName;
use crate::record_lsp::{record_hover_with_package_and_usage, signature_usage_from_facts};
use crate::record_lsp::{LspKnowledge, SignatureUsage};
use crate::source::SourceNavigation;

/// The hover at `position`: a local symbol renders its binding info and local
/// method signatures; a builtin/package object renders its record from the
/// partition that owns it (with call-context signature specialization).
pub fn hover_response(
    document: &DocumentSnapshot,
    position: Position,
    knowledge: &(impl LspKnowledge + ?Sized),
) -> Option<Hover> {
    let text = document.text();
    let analysis = document.analysis();

    if let Some(reference) = document.documentation_reference_at(position) {
        let name = reference.name(text);
        let mut hover = if let Some(symbol) = document.documentation_symbol(&reference) {
            local_symbol_hover(
                name,
                &symbol,
                analysis,
                document.callable_at_position(position),
                None,
                document
                    .documentation_for_binding(symbol)
                    .map(|documentation| documentation.hover_markdown()),
            )
        } else {
            let (package, record) =
                knowledge.get_record_with_package(&ObjectName(name.to_string()))?;
            record_hover_with_package_and_usage(record, Some(&package), knowledge, None)
        };
        hover.range = Some(reference.range());
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
            .map(|documentation| documentation.hover_markdown());
        return Some(local_symbol_hover(
            node_text,
            &symbol,
            analysis,
            local_method,
            pinned_signature,
            documentation,
        ));
    }

    // Render from the partition that owns the record so an imported package
    // object shows its own documentation/signatures, not only a Core lookup.
    let (package, record) =
        knowledge.get_record_with_package(&ObjectName(node_text.to_string()))?;
    let signature_usage =
        call_signature_usage_for_hover(node, node_text, document, analysis, knowledge);
    Some(record_hover_with_package_and_usage(
        record,
        Some(&package),
        knowledge,
        signature_usage.as_ref(),
    ))
}

/// The hover for a symbol resolved by the local analysis: its inferred type,
/// its role label, and (for method functions) its installed signatures.
fn local_symbol_hover(
    name: &str,
    symbol: &(impl Metadata + ?Sized),
    analysis: &Analysis,
    method: Option<&FunctionInfo>,
    pinned_signature: Option<&MethodInstallation>,
    documentation: Option<&str>,
) -> Hover {
    let meta = symbol.meta();
    let title_signature = method
        .zip(pinned_signature)
        .map(|(method, signature)| {
            format!(
                " `{}`",
                local_method_signature_label(method, &signature.method)
            )
        })
        .unwrap_or_default();
    let type_line = meta
        .type_label
        .as_deref()
        .map(|type_name| format!("\nType: `{type_name}`"))
        .unwrap_or_default();
    let label = match meta.symbol_kind {
        Some(SymbolKind::FUNCTION) if method.is_some() => "User-defined method function",
        Some(SymbolKind::FUNCTION) => "User-defined function",
        Some(SymbolKind::VARIABLE) if meta.binding_role == Some(BindingRole::Parameter) => {
            "Function parameter"
        }
        Some(SymbolKind::VARIABLE) => "User-defined binding",
        _ => "User-defined symbol",
    };
    let signatures = method
        .map(|method| local_method_signatures_markdown(analysis, method, pinned_signature))
        .unwrap_or_default();
    let documentation = documentation
        .map(|documentation| format!("\n\n{documentation}"))
        .unwrap_or_default();
    let markdown = format!(
        "**{}**{}{}\n\n{}{}{}",
        name, title_signature, type_line, label, signatures, documentation
    );

    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: markdown,
        }),
        range: None,
    }
}

/// The signature usage at a call site, for hover specialization: the argument
/// types at the enclosing application/operator, resolved against the loaded scope.
fn call_signature_usage_for_hover(
    node: M2Node,
    node_text: &str,
    source: &(impl SourceNavigation + ?Sized),
    analysis: &crate::analysis::Analysis,
    knowledge: &(impl LspKnowledge + ?Sized),
) -> Option<SignatureUsage> {
    let parent = node.parent()?;

    let facts = if parent.is_space_application() {
        let callable = parent.child_by_field_name("left")?;
        if callable.id() != node.id() {
            return None;
        }

        let argument = parent.child_by_field_name("right")?;
        analysis.infer_call_static_facts(argument, source, knowledge)
    } else if parent
        .child_by_field_name("operator")
        .is_some_and(|operator| operator.id() == node.id())
    {
        let left = parent.child_by_field_name("left")?;
        let right = parent.child_by_field_name("right")?;
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
fn hoverable_symbol_or_operator_node(node: M2Node) -> bool {
    if node.is_symbol_like() {
        return true;
    }

    node.is_operator()
}

fn local_method_signatures_markdown(
    analysis: &Analysis,
    method: &FunctionInfo,
    pinned_signature: Option<&MethodInstallation>,
) -> String {
    if let Some(pinned_signature) = pinned_signature {
        let mut lines = Vec::new();
        let excluded = analysis
            .methods_for(method)
            .filter(|signature| {
                signature.domain != pinned_signature.method.domain
                    || signature.codomain != pinned_signature.method.codomain
            })
            .collect::<Vec<_>>();
        if !excluded.is_empty() {
            lines.push("\n\n**Other signatures for this call:**".to_string());
            for signature in excluded.iter().take(15) {
                lines.push(format!(
                    "- `{}`",
                    local_method_signature_label(method, signature)
                ));
            }
            if excluded.len() > 15 {
                lines.push("- ...".to_string());
            }
        }
        return lines.join("\n");
    }

    if method.installations.is_empty() {
        return method
            .typical_value
            .as_ref()
            .map(|codomain| format!("\n\nCodomain: `{}`", codomain.name()))
            .unwrap_or_default();
    }

    let mut lines = vec!["\n\n**Methods:**".to_string()];
    for signature in analysis.methods_for(method) {
        lines.push(format!(
            "- `{}`",
            local_method_signature_label(method, signature)
        ));
    }
    lines.join("\n")
}

fn local_method_signature_label(method: &FunctionInfo, signature: &Method) -> String {
    let domain = format!(
        "({})",
        signature
            .domain
            .iter()
            .map(ObjectName::name)
            .collect::<Vec<_>>()
            .join(", ")
    );
    let codomain = signature
        .codomain
        .as_ref()
        .or(method.typical_value.as_ref())
        .map(|codomain| format!(" -> {}", codomain.name()))
        .unwrap_or_default();
    format!("{domain}{codomain}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::{BindingRole, Meta};
    use crate::node_metadata::M2Parser;
    use crate::object_registry::ObjectRegistry;
    use crate::source::DocumentSource;
    use crate::test_support::analyze;
    use tower_lsp::lsp_types::{HoverContents, Range as TextRange, SymbolKind};

    #[test]
    fn local_hover_includes_known_static_type() {
        let mut parser = M2Parser::new().expect("Macaulay2 parser should load");
        let analysis = analyze(parser.parse("").expect("empty fixture should parse"));
        let symbol = Meta {
            symbol_kind: Some(SymbolKind::VARIABLE),
            binding_role: Some(BindingRole::Ordinary),
            type_label: Some("Package".to_string()),
        };

        let hover = local_symbol_hover("Doc", &symbol, &analysis, None, None, None);
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("local hover should use markdown");
        };

        assert!(
            markup.value.starts_with("**Doc**\nType: `Package`"),
            "local hover should place known static type facts below the title name"
        );
        assert!(!markup.value.contains("Defined at"));
    }

    #[test]
    fn local_hover_includes_method_signatures() {
        let text = "p = method(TypicalValue => List)\np(ZZ, ZZ) := (i, j) -> {i, j}\n";
        let mut parser = M2Parser::new().expect("Macaulay2 parser should load");
        let analysis = analyze(parser.parse(text).expect("fixture should parse"));
        let symbol = analysis
            .get_binding_at("p", pos!(1, 0))
            .expect("method symbol should be visible");
        let method = analysis
            .function_at("p", pos!(1, 0))
            .expect("method should be registered");

        let hover = local_symbol_hover("p", &symbol, &analysis, Some(method), None, None);
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("local hover should use markdown");
        };

        assert!(markup.value.contains("User-defined method function"));
        assert!(markup.value.contains("`(ZZ, ZZ) -> List`"));
    }

    #[test]
    fn local_method_installation_hover_pins_installed_signature() {
        let text = "p = method(TypicalValue => List)\np(ZZ, ZZ) := (i, j) -> {i, j}\np(CC, CC) := Array => (i, j) -> [i, j]\n";
        let mut parser = M2Parser::new().expect("Macaulay2 parser should load");
        let tree = parser.parse_tree(text, None).expect("fixture should parse");
        let root = tree.root(text);
        let analysis = analyze(root);
        let source = DocumentSource::new(text.to_string());
        let position = pos!(1, 0);
        let node = root
            .descendant_for_point_range(
                tree_sitter::Point::new(1, 0),
                tree_sitter::Point::new(1, 0),
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
        );
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("local hover should use markdown");
        };

        assert!(markup
            .value
            .contains("**p** `(ZZ, ZZ) -> List`\nType: `MethodFunction`"));
        assert!(!markup.value.contains("**Signature:**"));
        assert!(markup.value.contains("**Other signatures for this call:**"));
        assert!(markup.value.contains("`(CC, CC) -> Array`"));
        assert!(!markup.value.contains("**Methods:**"));
    }

    #[test]
    fn repeated_domain_installations_pin_their_own_source_fact() {
        let text = concat!(
            "p = method()\n",
            "p ZZ := List => x -> x\n",
            "p ZZ := Array => x -> x\n",
        );
        let mut parser = M2Parser::new().expect("Macaulay2 parser should load");
        let tree = parser.parse_tree(text, None).expect("fixture should parse");
        let root = tree.root(text);
        let analysis = analyze(root);
        let source = DocumentSource::new(text.to_string());

        for (line, expected_codomain) in [(1, "List"), (2, "Array")] {
            let point = tree_sitter::Point::new(line, 0);
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
        let index = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let scoped = index.with_source_imports(text);
        let hover = hover_response(&document, pos!(1, 0), &scoped)
            .expect("hover over an imported package object");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("imported-package hover should use markdown");
        };
        assert!(
            markup.value.contains("Package: `JSON`"),
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
    fn hover_resolves_local_wikilink_documentation_references() {
        let text = "-- use [[x]]\nx := 1\n";
        let document = DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
            .expect("fixture should parse");
        let hover = hover_response(&document, pos!(0, 9), &ObjectRegistry::default())
            .expect("local documentation reference should have a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("local hover should use markdown");
        };

        assert!(markup.value.starts_with("**x**"));
        assert!(markup.value.contains("User-defined binding"));
        assert_eq!(hover.range, Some(TextRange::new(pos!(0, 9), pos!(0, 10))));
    }

    #[test]
    fn hover_resolves_indexed_wikilink_documentation_references() {
        let text = "-- use [[ideal]]\nx := 1\n";
        let document = DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
            .expect("fixture should parse");
        let index = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let scoped = index.with_source_imports(text);
        let hover = hover_response(&document, pos!(0, 9), &scoped)
            .expect("indexed documentation reference should have a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("indexed hover should use markdown");
        };

        assert!(
            markup.value.starts_with("**ideal**"),
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
        let hover = hover_response(&document, pos!(2, 2), &ObjectRegistry::default())
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
        let index = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let scoped = index.with_source_imports("");
        let mut parser = M2Parser::new().expect("Macaulay2 parser should load");
        let tree = parser.parse_tree(text, None).expect("fixture should parse");
        let root = tree.root(text);
        let source = DocumentSource::new(text.to_string());
        let analysis = Analysis::new_with_knowledge(root, None, &source, &scoped);
        let node = root
            .descendant_for_point_range(
                tree_sitter::Point::new(0, 5),
                tree_sitter::Point::new(0, 5),
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
        let index = ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let scoped = index.with_source_imports("");
        let mut parser = M2Parser::new().expect("Macaulay2 parser should load");
        let tree = parser.parse_tree(text, None).expect("fixture should parse");
        let root = tree.root(text);
        let source = DocumentSource::new(text.to_string());
        let analysis = Analysis::new_with_knowledge(root, None, &source, &scoped);
        let node = root
            .descendant_for_point_range(
                tree_sitter::Point::new(2, 7),
                tree_sitter::Point::new(2, 7),
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
