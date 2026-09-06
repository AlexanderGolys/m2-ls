//! Completion and navigation features: definition, references, rename, and
//! workspace-symbol queries.

use std::collections::HashMap;

use m2_syn::nodes::Symbol;
use tower_lsp::lsp_types::request::{
    GotoDeclarationResponse, GotoImplementationResponse, GotoTypeDefinitionResponse,
};
use tower_lsp::lsp_types::Range as TextRange;
use tower_lsp::lsp_types::*;

use crate::document::{DocumentSnapshot, TargetSymbol};
use crate::node_metadata::M2Node;
use crate::object_registry::ObjectName;
use crate::package_index::{package_source_string, SourceResolver};
use crate::record_lsp::LspKnowledge;
use crate::source::SourceNavigation;
use crate::workspace_index::{
    ImplementationKind, WorkspaceDefinitionKnowledge, WorkspaceImplementationKnowledge,
    WorkspaceSymbolKnowledge,
};

/// The in-file references of the symbol at `position` as LSP locations.
pub fn references_response(
    document: &DocumentSnapshot,
    uri: &Url,
    position: Position,
    include_declaration: bool,
) -> Vec<Location> {
    let ranges = if document.target_symbol_at(position).is_some() {
        collect_reference_ranges(document, position, include_declaration)
    } else {
        document
            .symbol_occurrence_at(position)
            .map(|(name, _)| unbound_reference_ranges(document, name))
            .unwrap_or_default()
    };
    ranges
        .into_iter()
        .map(|range| Location {
            uri: uri.clone(),
            range,
        })
        .collect()
}

#[allow(deprecated)]
pub fn workspace_symbols_response(
    query: &str,
    workspace: &(impl WorkspaceSymbolKnowledge + ?Sized),
) -> Vec<SymbolInformation> {
    workspace
        .matching_symbols(query)
        .into_iter()
        .map(|symbol| SymbolInformation {
            name: symbol.name.to_string(),
            kind: symbol.kind,
            tags: None,
            deprecated: None,
            location: symbol.location,
            container_name: None,
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum NavigationTarget {
    Declaration,
    Definition,
}

pub fn goto_declaration_response(
    document: &DocumentSnapshot,
    uri: &Url,
    position: Position,
    knowledge: &(impl LspKnowledge + ?Sized),
    source_resolver: &SourceResolver,
    workspace_index: &(impl WorkspaceDefinitionKnowledge + ?Sized),
) -> Option<GotoDeclarationResponse> {
    goto_symbol_response(
        document,
        uri,
        position,
        knowledge,
        source_resolver,
        workspace_index,
        NavigationTarget::Declaration,
    )
}

pub fn goto_definition_response(
    document: &DocumentSnapshot,
    uri: &Url,
    position: Position,
    knowledge: &(impl LspKnowledge + ?Sized),
    source_resolver: &SourceResolver,
    workspace_index: &(impl WorkspaceDefinitionKnowledge + ?Sized),
) -> Option<GotoDefinitionResponse> {
    goto_symbol_response(
        document,
        uri,
        position,
        knowledge,
        source_resolver,
        workspace_index,
        NavigationTarget::Definition,
    )
}

fn goto_symbol_response(
    document: &DocumentSnapshot,
    uri: &Url,
    position: Position,
    knowledge: &(impl LspKnowledge + ?Sized),
    source_resolver: &SourceResolver,
    workspace_index: &(impl WorkspaceDefinitionKnowledge + ?Sized),
    target: NavigationTarget,
) -> Option<GotoDefinitionResponse> {
    let node = document.node_at_position_minimal(position)?;
    let documentation_reference = document.documentation_reference_at(position);

    if let NavigationTarget::Definition = target {
        if let Some(string_node) = node.enclosing_node(M2Node::is_string_literal) {
            if let Some(package_name) = crate::package_index::package_source_string(string_node) {
                if let Some(path) = source_resolver.resolve_package_file(package_name) {
                    if let Ok(uri) = Url::from_file_path(path) {
                        return Some(GotoDefinitionResponse::Scalar(Location {
                            uri,
                            range: TextRange::new(pos!(), pos!()),
                        }));
                    }
                }
            }
        }
    }

    let node_text = if let Some(reference) = documentation_reference.as_ref() {
        reference.name(document.text())
    } else {
        if !node.is::<Symbol>() {
            return None;
        }
        node.text()
    };

    let local_binding = match target {
        NavigationTarget::Declaration => document
            .binding_at_position(position)
            .or_else(|| document.future_assignment_binding_at(node_text, position)),
        NavigationTarget::Definition => document
            .source_binding_at(node_text, position)
            .or_else(|| document.future_assignment_binding_at(node_text, position)),
    };
    if let Some(binding) = local_binding {
        let ranges = match target {
            NavigationTarget::Declaration => vec![binding.range],
            NavigationTarget::Definition => binding
                .states
                .iter()
                .map(|state| state.span)
                .collect::<Vec<_>>(),
        };
        return locations_response(ranges.into_iter().map(|range| Location {
            uri: uri.clone(),
            range,
        }));
    }

    let workspace_locations = match target {
        NavigationTarget::Declaration => workspace_index.declarations(node_text, uri),
        NavigationTarget::Definition => workspace_index.definitions(node_text, uri),
    };
    if !workspace_locations.is_empty() {
        return locations_response(workspace_locations);
    }

    match target {
        NavigationTarget::Declaration => unbound_reference_ranges(document, node_text)
            .into_iter()
            .next()
            .map(|range| {
                GotoDefinitionResponse::Scalar(Location {
                    uri: uri.clone(),
                    range,
                })
            }),
        NavigationTarget::Definition => {
            let (package, _) =
                knowledge.get_record_with_package(&ObjectName(node_text.to_string()))?;
            source_resolver
                .package_location(&package)
                .map(GotoDefinitionResponse::Scalar)
        }
    }
}

pub fn goto_type_definition_response(
    document: &DocumentSnapshot,
    uri: &Url,
    position: Position,
    knowledge: &(impl LspKnowledge + ?Sized),
    workspace_index: &(impl WorkspaceDefinitionKnowledge + ?Sized),
    source_resolver: &SourceResolver,
) -> Option<GotoTypeDefinitionResponse> {
    let node = document.symbol_node_at_position(position)?;
    let inferred_type = document
        .binding_at_position(position)
        .and_then(|binding| binding.state.inferred_type.as_ref())
        .and_then(crate::typesystem::InferredType::single)
        .cloned()
        .or_else(|| {
            document
                .analysis()
                .infer_expression_static_type(node, document, knowledge)
        })?;
    if inferred_type.name() == "Thing" {
        return None;
    }

    if let Some(binding) = document.source_binding_at(inferred_type.name(), position) {
        if binding.state.source_type.is_some() {
            return locations_response([Location {
                uri: uri.clone(),
                range: binding.range,
            }]);
        }
    }

    let workspace_locations = workspace_index.type_definitions(inferred_type.name(), uri);
    if !workspace_locations.is_empty() {
        return locations_response(workspace_locations);
    }

    let (package, record) = knowledge.get_record_with_package(&inferred_type)?;
    record.type_info()?;
    source_resolver
        .package_location(&package)
        .map(GotoDefinitionResponse::Scalar)
}

pub fn goto_implementation_response(
    document: &DocumentSnapshot,
    position: Position,
    uri: &Url,
    knowledge: &(impl LspKnowledge + ?Sized),
    workspace_index: &(impl WorkspaceImplementationKnowledge + ?Sized),
) -> Option<GotoImplementationResponse> {
    let (name, _) = document.symbol_occurrence_at(position)?;
    let analysis = document.analysis();
    let binding = document
        .binding_at_position(position)
        .or_else(|| document.future_assignment_binding_at(name, position));
    let function = binding.and_then(|binding| analysis.function_for_binding(binding));
    let call_installations = function.and_then(|function| {
        let callable = document.symbol_node_at_position(position)?;
        if analysis.is_method_installation_callable(callable, document) {
            return None;
        }
        let application = callable.parent()?;
        if !application.is_space_application() {
            return None;
        }
        let head = application.child_by_field_name("left")?;
        (head.id() == callable.id()).then_some(())?;
        let argument = application.child_by_field_name("right")?;
        Some(analysis.local_call_installations(
            function,
            argument,
            document.position_for_node(callable),
            document,
            knowledge,
        ))
    });
    let pinned_installation = call_installations
        .as_ref()
        .filter(|installations| installations.len() == 1)
        .and_then(|installations| installations.first().copied());
    let local_binding = binding.is_some_and(|binding| binding.scope_idx != 0);
    let indexed_method = binding.is_none()
        && knowledge
            .get_record(&ObjectName::new(name))
            .and_then(|record| record.callable())
            .is_some_and(|callable| callable.is_method_function());
    let workspace_method_declared =
        !local_binding && workspace_index.has_method_declaration(name, uri);

    let named_method_ranges = analysis
        .assignment_facts()
        .iter()
        .filter_map(|assignment| {
            let crate::analysis::AssignmentFactKind::MethodInstallation(id) = assignment.kind
            else {
                return None;
            };
            let installation = analysis.method_installation(id)?;
            (assignment.scope_idx == 0
                && (installation.takes_effect()
                    || installation.is_workspace_candidate() && workspace_method_declared)
                && installation.method.head.name().name() == name)
                .then_some(assignment.target_span)
        })
        .collect::<Vec<_>>();
    let workspace_method_locations = if local_binding {
        Vec::new()
    } else {
        workspace_index.implementations(name, ImplementationKind::Method, uri)
    };
    let method_function = function.is_some_and(|function| function.is_method_function())
        || indexed_method
        || (binding.is_none()
            && (!named_method_ranges.is_empty() || !workspace_method_locations.is_empty()));

    let mut locations = if method_function {
        let ranges = function.map_or(named_method_ranges, |function| {
            analysis
                .assignment_facts()
                .iter()
                .filter_map(|assignment| {
                    let crate::analysis::AssignmentFactKind::MethodInstallation(id) =
                        assignment.kind
                    else {
                        return None;
                    };
                    let included = call_installations.as_ref().map_or_else(
                        || function.installations.contains(&id),
                        |installations| {
                            installations
                                .iter()
                                .any(|installation| installation.id == id)
                        },
                    );
                    included.then_some(assignment.target_span)
                })
                .collect()
        });
        let mut locations = ranges
            .into_iter()
            .map(|range| Location {
                uri: uri.clone(),
                range,
            })
            .collect::<Vec<_>>();
        if pinned_installation.is_none() {
            locations.extend(workspace_method_locations);
        }
        locations
    } else {
        let lambda_ranges = document.lambda_value_ranges();
        let ranges = binding.map_or_else(Vec::new, |binding| {
            binding
                .states
                .iter()
                .filter(|state| {
                    state
                        .value_range
                        .is_some_and(|range| lambda_ranges.contains(&range))
                })
                .map(|state| state.span)
                .collect::<Vec<_>>()
        });
        let mut locations = ranges
            .into_iter()
            .map(|range| Location {
                uri: uri.clone(),
                range,
            })
            .collect::<Vec<_>>();
        if !local_binding {
            locations.extend(workspace_index.implementations(
                name,
                ImplementationKind::Lambda,
                uri,
            ));
        }
        locations
    };

    locations.sort_by(|left, right| {
        left.uri
            .as_str()
            .cmp(right.uri.as_str())
            .then_with(|| left.range.start.cmp(&right.range.start))
    });
    locations.dedup();
    locations_response(locations)
}

pub fn document_links_response(document: &DocumentSnapshot, uri: &Url) -> Vec<DocumentLink> {
    let mut links = document
        .documentation_references()
        .iter()
        .map(|reference| {
            document_link(
                reference.range(),
                uri,
                format!("Open `{}`", reference.name(document.text())),
            )
        })
        .collect::<Vec<_>>();

    links.extend(
        document
            .root_node()
            .descendants()
            .filter(M2Node::is_string_literal)
            .filter_map(|node| {
                let package = package_source_string(node)?;
                Some(document_link(
                    document.range_for_node(node),
                    uri,
                    format!("Open package `{package}`"),
                ))
            }),
    );
    links.sort_by_key(|link| (link.range.start, link.range.end));
    links.dedup_by_key(|link| link.range);
    links
}

pub fn document_link_request(link: &DocumentLink) -> Option<TextDocumentPositionParams> {
    serde_json::from_value(link.data.clone()?).ok()
}

pub fn resolve_document_link(
    mut link: DocumentLink,
    response: GotoDefinitionResponse,
) -> DocumentLink {
    let location = match response {
        GotoDefinitionResponse::Scalar(location) => Some(location),
        GotoDefinitionResponse::Array(locations) => locations.into_iter().next(),
        GotoDefinitionResponse::Link(links) => links.into_iter().next().map(|link| Location {
            uri: link.target_uri,
            range: link.target_selection_range,
        }),
    };
    if let Some(location) = location {
        let mut target = location.uri;
        if location.range.start != pos!() {
            target.set_fragment(Some(&format!(
                "L{},{}",
                location.range.start.line + 1,
                location.range.start.character + 1
            )));
        }
        link.target = Some(target);
        link.data = None;
    }
    link
}

fn document_link(range: TextRange, uri: &Url, tooltip: String) -> DocumentLink {
    let request =
        TextDocumentPositionParams::new(TextDocumentIdentifier::new(uri.clone()), range.start);
    DocumentLink {
        range,
        target: None,
        tooltip: Some(tooltip),
        data: serde_json::to_value(request).ok(),
    }
}

fn locations_response(
    locations: impl IntoIterator<Item = Location>,
) -> Option<GotoDefinitionResponse> {
    let mut locations = locations.into_iter().collect::<Vec<_>>();
    match locations.len() {
        0 => None,
        1 => Some(GotoDefinitionResponse::Scalar(locations.remove(0))),
        _ => Some(GotoDefinitionResponse::Array(locations)),
    }
}

/// Every in-file range referring to the same binding as the symbol at
/// `position` (scope-aware: shadowed names in other scopes are excluded).
pub fn collect_reference_ranges(
    document: &DocumentSnapshot,
    position: Position,
    include_declaration: bool,
) -> Vec<TextRange> {
    let Some(target) = document.target_symbol_at(position) else {
        return Vec::new();
    };
    reference_ranges_resolved(target, document, include_declaration)
}

/// The reference-range collection step, given a pre-resolved target. Split out
/// of [`collect_reference_ranges`] so a caller that has already resolved the
/// target (e.g. document highlight, which also needs the declaration range) can
/// reuse it instead of resolving a second time.
pub fn reference_ranges_resolved(
    target: TargetSymbol<'_>,
    document: &DocumentSnapshot,
    include_declaration: bool,
) -> Vec<TextRange> {
    let target_name = target.name;
    let target_range = target.symbol.range;

    let mut references = Vec::new();
    for node in document.root_node().symbols() {
        let node_text = node.text();
        if node_text == target_name {
            let range = document.range_for_node(node);
            if let Some(symbol) = document.source_symbol_at(node_text, range.start) {
                if symbol.range == target_range && (include_declaration || range != target_range) {
                    references.push(range);
                }
            }
        }
    }
    for reference in document.documentation_references() {
        if reference.name(document.text()) != target_name {
            continue;
        }
        let range = reference.range();
        if let Some(symbol) = document.documentation_symbol(reference) {
            if symbol.range == target_range && (include_declaration || range != target_range) {
                references.push(range);
            }
        }
    }

    references.sort_by_key(|range| (range.start, range.end));
    references.dedup();
    references
}

/// The range of the symbol that would be renamed at `position`, or `None` if the
/// position is not on a renameable symbol. Only user symbols with a resolvable
/// binding qualify — builtins and package symbols are excluded, since renaming
/// them in this file alone would silently break the calls they stand for.
pub fn prepare_rename_range(document: &DocumentSnapshot, position: Position) -> Option<TextRange> {
    let target = document.target_symbol_at(position)?;
    Some(target.range)
}

/// A workspace edit renaming every in-file reference of the symbol at `position`
/// (including its declaration) to `new_name`, or `None` if there is nothing to
/// rename. References are resolved scope-aware via [`collect_reference_ranges`],
/// so shadowed names in other scopes are left untouched.
pub fn rename_edits(
    document: &DocumentSnapshot,
    uri: &Url,
    position: Position,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    if new_name.trim().is_empty() {
        return None;
    }
    let ranges = collect_reference_ranges(document, position, true);
    if ranges.is_empty() {
        return None;
    }
    let edits = ranges
        .into_iter()
        .map(|range| TextEdit {
            range,
            new_text: new_name.to_string(),
        })
        .collect();
    Some(WorkspaceEdit {
        changes: Some(HashMap::from([(uri.clone(), edits)])),
        document_changes: None,
        change_annotations: None,
    })
}

/// What a references request at `position` targets. A local binding's references
/// stay in the document; a global (top-level / workspace) symbol's references
/// span every file.
pub enum ReferenceTarget {
    Local,
    Global(String),
}

/// Classify the symbol at `position`: a binding in an inner scope is `Local`; a
/// top-level binding, or an undefined-here name that is defined at top level
/// elsewhere in the workspace, is `Global`.
pub fn reference_target(
    document: &DocumentSnapshot,
    position: Position,
    workspace_index: &(impl WorkspaceDefinitionKnowledge + ?Sized),
) -> Option<ReferenceTarget> {
    let (name, _) = document.symbol_occurrence_at(position)?;
    let name = name.to_string();
    match document.binding_at_position(position) {
        Some(binding) if binding.scope_idx != 0 => Some(ReferenceTarget::Local),
        Some(_) => Some(ReferenceTarget::Global(name)),
        None => workspace_index
            .is_defined(&name)
            .then_some(ReferenceTarget::Global(name)),
    }
}

/// Every occurrence of `name` in `document` that refers to a global (top-level)
/// definition — i.e. one not shadowed by a local binding at that point. Used to
/// gather a workspace-global symbol's references file by file.
pub fn global_reference_ranges(document: &DocumentSnapshot, name: &str) -> Vec<TextRange> {
    let analysis = document.analysis();
    let mut references = Vec::new();
    for node in document.root_node().symbols() {
        if node.text() == name {
            let position = document.position_for_node(node);
            // A use is global unless a local binding shadows the name here.
            let shadowed = analysis
                .get_binding_at(name, position)
                .is_some_and(|binding| binding.scope_idx != 0);
            if !shadowed {
                references.push(document.range_for_node(node));
            }
        }
    }
    for reference in document.documentation_references() {
        if reference.name(document.text()) != name {
            continue;
        }
        let range = reference.range();
        let shadowed = document
            .documentation_symbol(reference)
            .is_some_and(|binding| binding.scope_idx != 0);
        if !shadowed {
            references.push(range);
        }
    }
    references.sort_by_key(|range| (range.start, range.end));
    references.dedup();
    references
}

/// Every occurrence of `name` that is not bound by user code at that source
/// position. Used for in-document builtin highlighting: local shadows must not
/// light up with the library object they replace.
pub fn unbound_reference_ranges(document: &DocumentSnapshot, name: &str) -> Vec<TextRange> {
    let analysis = document.analysis();
    let mut references = document
        .root_node()
        .symbols()
        .filter(|node| node.text() == name)
        .filter_map(|node| {
            let range = document.range_for_node(node);
            analysis
                .get_binding_at(name, range.start)
                .is_none()
                .then_some(range)
        })
        .collect::<Vec<_>>();

    references.extend(
        document
            .documentation_references()
            .iter()
            .filter(|reference| reference.name(document.text()) == name)
            .filter_map(|reference| {
                let range = reference.range();
                document
                    .documentation_symbol(reference)
                    .is_none()
                    .then_some(range)
            }),
    );
    references.sort_by_key(|range| (range.start, range.end));
    references.dedup();
    references
}

/// Whether `name` is a valid M2 identifier (a letter followed by letters and
/// digits). A rename target failing this would silently produce unparsable
/// code, so the rename request must reject it instead of editing.
pub fn is_valid_m2_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocumentSnapshot;
    use crate::object_registry::ObjectRegistry;
    use crate::workspace_index::WorkspaceIndex;
    use tower_lsp::lsp_types::Range as TextRange;

    fn document(text: &str) -> DocumentSnapshot {
        DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
            .expect("fixture should parse")
    }

    #[test]
    fn m2_identifier_validation_accepts_plain_identifiers_only() {
        assert!(is_valid_m2_identifier("foo"));
        assert!(is_valid_m2_identifier("foo42"));
        assert!(!is_valid_m2_identifier("42foo"));
        assert!(!is_valid_m2_identifier("a b"));
        assert!(!is_valid_m2_identifier(""));
    }

    #[test]
    fn collect_reference_ranges_finds_same_file_local_symbols() {
        let text = "f := x -> (y := x + x; y)\nf 1";
        let document = document(text);

        let with_declaration = collect_reference_ranges(&document, pos!(0, 16), true);
        let without_declaration = collect_reference_ranges(&document, pos!(0, 16), false);

        assert_eq!(
            with_declaration,
            vec![
                TextRange::new(pos!(0, 5), pos!(0, 6)),
                TextRange::new(pos!(0, 16), pos!(0, 17)),
                TextRange::new(pos!(0, 20), pos!(0, 21)),
            ]
        );
        assert_eq!(
            without_declaration,
            vec![
                TextRange::new(pos!(0, 16), pos!(0, 17)),
                TextRange::new(pos!(0, 20), pos!(0, 21)),
            ]
        );
    }

    #[test]
    fn collect_reference_ranges_finds_forward_references_through_closures() {
        // `h` is used in `g`'s body before `h` is defined. M2 closures are
        // late-bound, so the use binds to the later top-level `h`; rename and
        // highlight must include it.
        let text = "g := x -> h x\nh := y -> y\n";
        let document = document(text);

        // Resolve from the declaration of `h` (line 1).
        let ranges = collect_reference_ranges(&document, pos!(1, 0), true);

        assert!(
            ranges.contains(&TextRange::new(pos!(0, 10), pos!(0, 11))),
            "forward reference to `h` in g's body must be collected, got {ranges:?}"
        );
        assert!(
            ranges.contains(&TextRange::new(pos!(1, 0), pos!(1, 1))),
            "the declaration of `h` must be collected, got {ranges:?}"
        );
    }

    #[test]
    fn wikilink_documentation_mentions_are_scope_aware_references() {
        let text = "x := 1\n-- use [[x]]\ny := x\n";
        let document = document(text);
        let ranges = collect_reference_ranges(&document, pos!(1, 9), true);

        assert_eq!(
            ranges,
            vec![
                TextRange::new(pos!(), pos!(0, 1)),
                TextRange::new(pos!(1, 9), pos!(1, 10)),
                TextRange::new(pos!(2, 5), pos!(2, 6)),
            ]
        );
        assert!(matches!(
            reference_target(
                &document,
                pos!(1, 9),
                &WorkspaceIndex::default()
            ),
            Some(ReferenceTarget::Global(name)) if name == "x"
        ));
    }

    #[test]
    fn wikilink_documentation_mentions_resolve_later_bindings() {
        let text = "-- use [[x]]\nx := 1\nx\n";
        let document = document(text);
        let ranges = collect_reference_ranges(&document, pos!(0, 9), true);

        assert_eq!(
            ranges,
            vec![
                TextRange::new(pos!(0, 9), pos!(0, 10)),
                TextRange::new(pos!(1, 0), pos!(1, 1)),
                TextRange::new(pos!(2, 0), pos!(2, 1)),
            ]
        );
        assert!(matches!(
            reference_target(
                &document,
                pos!(0, 9),
                &WorkspaceIndex::default()
            ),
            Some(ReferenceTarget::Global(name)) if name == "x"
        ));
    }

    #[test]
    fn goto_definition_does_not_jump_forward_from_documentation() {
        let text = "-- use [[x]]\nx := 1\n";
        let document = document(text);
        let index =
            crate::object_registry::ObjectRegistry::load(include_str!("../data/m2-index.jsonl"));
        let scoped = index.with_source_imports(text);
        let uri = Url::parse("file:///t.m2").expect("uri");

        assert_eq!(
            goto_definition_response(
                &document,
                &uri,
                pos!(0, 9),
                &scoped,
                &SourceResolver::new(Vec::new()),
                &WorkspaceIndex::default(),
            ),
            None
        );
    }

    #[test]
    fn rename_from_code_updates_wikilink_documentation_mentions() {
        let text = "f := x -> (\n-- Alias docs: [[x]]\ny := x;\ny + x)\n";
        let document = document(text);
        let uri = Url::parse("file:///t.m2").expect("uri");
        let edits = rename_edits(&document, &uri, pos!(0, 5), "value")
            .expect("parameter should be renameable")
            .changes
            .expect("simple changes")[&uri]
            .iter()
            .map(|edit| edit.range)
            .collect::<Vec<_>>();

        assert_eq!(
            edits,
            vec![
                TextRange::new(pos!(0, 5), pos!(0, 6)),
                TextRange::new(pos!(1, 17), pos!(1, 18)),
                TextRange::new(pos!(2, 5), pos!(2, 6)),
                TextRange::new(pos!(3, 4), pos!(3, 5)),
            ]
        );
    }

    #[test]
    fn reference_ranges_use_lsp_utf16_columns() {
        let text = "f := x -> (\"😀\"; x + x)";
        let document = document(text);

        let ranges = collect_reference_ranges(&document, pos!(0, 17), true);

        assert_eq!(
            ranges,
            vec![
                TextRange::new(pos!(0, 5), pos!(0, 6)),
                TextRange::new(pos!(0, 17), pos!(0, 18)),
                TextRange::new(pos!(0, 21), pos!(0, 22)),
            ]
        );
    }

    #[test]
    fn rename_edits_replace_every_in_scope_reference() {
        let text = "f := x -> (y := x + x; y)\nf 1";
        let document = document(text);
        let uri = Url::parse("file:///test.m2").expect("uri");

        // Cursor on a use of `x`; rename to `z`.
        let edit = rename_edits(&document, &uri, pos!(0, 16), "z")
            .expect("local symbol should be renameable");
        let edits = &edit.changes.expect("simple changes")[&uri];

        // Declaration + both uses, all rewritten to the new name.
        assert_eq!(
            edits.iter().map(|e| e.range).collect::<Vec<_>>(),
            vec![
                TextRange::new(pos!(0, 5), pos!(0, 6)),
                TextRange::new(pos!(0, 16), pos!(0, 17)),
                TextRange::new(pos!(0, 20), pos!(0, 21)),
            ]
        );
        assert!(edits.iter().all(|e| e.new_text == "z"));
    }

    #[test]
    fn prepare_rename_accepts_user_symbols_and_rejects_others() {
        let text = "f := x -> (x + x)";
        let document = document(text);
        // On the parameter `x`.
        assert!(prepare_rename_range(&document, pos!(0, 5)).is_some());
        // Empty new name is refused.
        let uri = Url::parse("file:///t.m2").expect("uri");
        assert!(rename_edits(&document, &uri, pos!(0, 5), "  ").is_none());
    }

    #[test]
    fn rename_includes_quoted_symbol_reference() {
        // `symbol M` names the identifier `M` (a plain `symbol` node); renaming the
        // user-defined `M` must rewrite that occurrence too.
        let text = "f := M -> (symbol M; M + 1)";
        let document = document(text);
        let uri = Url::parse("file:///t.m2").expect("uri");
        let edits = rename_edits(&document, &uri, pos!(0, 5), "N")
            .expect("user symbol should be renameable")
            .changes
            .expect("simple changes")[&uri]
            .iter()
            .map(|edit| edit.range.start.character)
            .collect::<Vec<_>>();
        // parameter `M`, the `M` in `symbol M`, and the `M` in `M + 1`.
        assert_eq!(edits, vec![5, 18, 21]);
    }

    #[test]
    fn rename_rejects_symbols_not_defined_by_the_user() {
        let uri = Url::parse("file:///t.m2").expect("uri");
        // `Algorithm` is a global reserved option key, not a user definition.
        let opt = document("g := gens gb(I, Algorithm => Homogeneous)");
        assert!(rename_edits(&opt, &uri, pos!(0, 16), "Z").is_none());
        assert!(prepare_rename_range(&opt, pos!(0, 16)).is_none());
        // Keywords and punctuation that the grammar resolves into symbols.
        let kw = document("z = a and b");
        assert!(rename_edits(&kw, &uri, pos!(0, 6), "Z").is_none());
        let brace = document("x = {1, 2}");
        assert!(rename_edits(&brace, &uri, pos!(0, 4), "Z").is_none());
    }

    #[test]
    fn reference_target_classifies_local_versus_global() {
        struct KnownGlobal;

        impl WorkspaceDefinitionKnowledge for KnownGlobal {
            fn declarations(&self, _name: &str, _exclude: &Url) -> Vec<Location> {
                Vec::new()
            }

            fn definitions(&self, _name: &str, _exclude: &Url) -> Vec<Location> {
                Vec::new()
            }

            fn type_definitions(&self, _name: &str, _exclude: &Url) -> Vec<Location> {
                Vec::new()
            }

            fn is_defined(&self, name: &str) -> bool {
                name == "shared"
            }

            fn semantic_token_type(
                &self,
                _name: &str,
                _exclude: &Url,
            ) -> Option<crate::semantic_token::M2SemanticTokenType> {
                None
            }
        }

        let index = crate::workspace_index::WorkspaceIndex::default();
        // Lambda parameter -> local (references stay in-file).
        let local = document("f := x -> (x + x)");
        assert!(matches!(
            reference_target(&local, pos!(0, 5), &index),
            Some(ReferenceTarget::Local)
        ));
        // Top-level binding -> global.
        let global = document("y = 5\nz = y");
        assert!(matches!(
            reference_target(&global, pos!(), &index),
            Some(ReferenceTarget::Global(name)) if name == "y"
        ));
        let external = document("shared");
        assert!(matches!(
            reference_target(&external, pos!(), &KnownGlobal),
            Some(ReferenceTarget::Global(name)) if name == "shared"
        ));
    }

    #[test]
    fn global_reference_ranges_skip_local_shadows() {
        // `g` is a top-level global; `f`'s parameter `g` shadows it inside `f`.
        let document = document("g = 1\nf := g -> (g + g)\nh = g + g");
        let ranges = global_reference_ranges(&document, "g");
        // The global definition and its top-level uses, not the shadowed uses.
        assert_eq!(
            ranges,
            vec![
                TextRange::new(pos!(), pos!(0, 1)),
                TextRange::new(pos!(2, 4), pos!(2, 5)),
                TextRange::new(pos!(2, 8), pos!(2, 9)),
            ]
        );
    }
}
