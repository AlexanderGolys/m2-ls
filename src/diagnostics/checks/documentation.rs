//! The documentation occasion: diagnostics about the package documentation a
//! document writes, in either format (see [`crate::package_documentation`]).
//!
//! What `document` and `SimpleDoc` reject or warn about while reading one page
//! arrives as the documentation's faults and is worded here. The remaining
//! checks need the whole file: keys documented twice, names the documentation
//! evaluates that the package never defines, exports left undocumented, and
//! documentation where Macaulay2 cannot read it.
//!
//! A package may keep code or documentation in files it loads, which this
//! document cannot see. The checks that need every definition or every page
//! of a package therefore run only in a package section that loads no other
//! file.

use std::collections::{HashMap, HashSet};

use strum::IntoEnumIterator;

use tower_lsp::lsp_types::Position;

use super::{DiagnosticSink, DocumentationContext};
use crate::diagnostics::{catalog, Finding};
use crate::file_load::FileLoad;
use crate::macaulay2::Macaulay2Conventions;
use crate::node_metadata::SyntaxNode;
use crate::object_registry::ObjectName;
use crate::package_documentation::{
    DocumentOption, DocumentTag, DocumentationFault, DocumentationFaultKind, DocumentationKey,
    DocumentedSubject, SimpleDocKeyword,
};
use crate::package_index::collect_imported_packages_in_tree;
use crate::source::SourceNavigation;
use crate::source_package::PackageDeclaration;

/// The package that defines `doc` and the other SimpleDoc functions, which
/// `beginDocumentation()` loads last.
const SIMPLE_DOC_PACKAGE: &str = Macaulay2Conventions::DOCUMENTATION_PACKAGES[1];

/// Spawns every diagnostic about the document's package documentation.
pub(super) fn spawn(context: &DocumentationContext, sink: &mut DiagnosticSink) {
    for fault in context.documentation.faults() {
        fault_finding(fault, sink);
    }
    let file = PackageFile::read(context);
    duplicate_documentation(context, sink);
    documentation_outside_package(context, sink);
    documentation_before_begin(context, sink);
    undefined_documentation_key(context, &file, sink);
    undocumented_export(context, &file, sink);
}

/// Word a fault `document` or `SimpleDoc` reports while reading a page.
fn fault_finding(fault: &DocumentationFault, sink: &mut DiagnosticSink) {
    use DocumentationFaultKind::*;
    let range = fault.range;
    let (rule, message) = match &fault.kind {
        UnknownSimpleDocKeyword { keyword, expected } => (
            &catalog::UNKNOWN_DOCUMENTATION_SECTION,
            format!(
                "`{keyword}` is not a SimpleDoc keyword here; a section starts with one of {}. \
                 Indent content lines deeper than the keyword they belong to.",
                expected
                    .iter()
                    .map(SimpleDocKeyword::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        UnknownDocumentOption { option }
            if option.parse() == Ok(SimpleDocKeyword::Description) =>
        {
            (
            &catalog::UNKNOWN_DOCUMENTATION_SECTION,
                "`document` has no `Description` option: put the description's hypertext in \
                 the list itself, after the options"
                    .to_string(),
            )
        }
        UnknownDocumentOption { option } => (
            &catalog::UNKNOWN_DOCUMENTATION_SECTION,
            format!(
                "`{option}` is not a `document` option; the options are {}",
                DocumentOption::iter()
                    .map(|known| known.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        RepeatedSection { section } => (
            &catalog::MALFORMED_DOCUMENTATION,
            format!("`{section}` is given twice in this page; Macaulay2 rejects a repeated documentation option, so merge the two"),
        ),
        EmptySection {
            section,
            expectation,
        } => (
            &catalog::MALFORMED_DOCUMENTATION,
            format!("`{section}` expects {expectation}, indented below it"),
        ),
        MultilineHeadline { section } => (
            &catalog::MALFORMED_DOCUMENTATION,
            format!("`{section}` takes a single indented line; move further text into the description"),
        ),
        MalformedItem => (
            &catalog::MALFORMED_DOCUMENTATION,
            "an `Inputs`/`Outputs` item names the item and its type around one `:` or `=>`, \
             as in `n:ZZ -- the size` or `Limit => ZZ -- the bound`"
                .to_string(),
        ),
        UnmatchedAt => (
            &catalog::MALFORMED_DOCUMENTATION,
            "unmatched `@`: `@…@` encloses a Macaulay2 expression such as `@TO f@`; write `\\@` \
             for a literal `@`"
                .to_string(),
        ),
        MalformedExpression => (
            &catalog::MALFORMED_DOCUMENTATION,
            "this does not parse as Macaulay2, yet SimpleDoc evaluates it as Macaulay2 code"
                .to_string(),
        ),
        MixedNodes => (
            &catalog::MALFORMED_DOCUMENTATION,
            "a SimpleDoc string is either one page or a list of `Node` sections; move these \
             sections into a `Node`"
                .to_string(),
        ),
        OptionsOutsideList => (
            &catalog::MALFORMED_DOCUMENTATION,
            "`document` takes its options inside its list: write `document {Key => …, …}`"
                .to_string(),
        ),
        ExpectedList { option } => (
            &catalog::MALFORMED_DOCUMENTATION,
            format!("`{option} =>` expects a list, such as `{{\"n\" => ZZ}}`"),
        ),
        UsageRequired => (
            &catalog::MALFORMED_DOCUMENTATION,
            "`Inputs` and `Outputs` describe a `Usage`, which this page lacks; add a `Usage` \
             section such as `y = f x`"
                .to_string(),
        ),
        MissingKey => (
            &catalog::MISSING_DOCUMENTATION_KEY,
            "documentation without a `Key`; name what it documents with a `Key` section or \
             `Key => …`"
                .to_string(),
        ),
        HeadlineTooLong { length } if *length > Macaulay2Conventions::HEADLINE_MAXIMUM_LENGTH => (
            &catalog::MALFORMED_DOCUMENTATION,
            format!(
                "headline of {length} characters; Macaulay2 rejects headlines over {} characters, \
                 so move the detail into the description",
                Macaulay2Conventions::HEADLINE_MAXIMUM_LENGTH
            ),
        ),
        HeadlineTooLong { length } => (
            &catalog::LONG_HEADLINE,
            format!(
                "headline of {length} characters; Macaulay2 warns about headlines over {} \
                 characters, so move the detail into the description",
                Macaulay2Conventions::HEADLINE_WARNING_LENGTH
            ),
        ),
    };
    sink.spawn(rule, Finding::new(range, message));
}

/// Report every key documented after it was documented or marked
/// `undocumented` already: Macaulay2 keeps one page per tag.
fn duplicate_documentation(context: &DocumentationContext, sink: &mut DiagnosticSink) {
    let documentation = context.documentation;
    let mut keys = documentation
        .nodes()
        .iter()
        .flat_map(|node| node.keys.iter())
        .chain(documentation.undocumented())
        .collect::<Vec<_>>();
    keys.sort_by_key(|key| key.range.start);
    let mut first_by_tag = HashMap::<DocumentTag, &DocumentationKey>::new();
    for key in keys {
        let tag = key.subject.tag();
        match first_by_tag.get(&tag) {
            Some(first) => sink.spawn(
                &catalog::DUPLICATE_DOCUMENTATION,
                Finding::new(
                    key.range,
                    format!(
                        "`{}` is documented twice; Macaulay2 keeps one page per key, so merge \
                         the pages",
                        tag.as_str()
                    ),
                )
                .with_note(first.range, "first documented here"),
            ),
            None => {
                first_by_tag.insert(tag, key);
            }
        }
    }
}

/// Report pages outside every package section of a file declaring packages.
fn documentation_outside_package(context: &DocumentationContext, sink: &mut DiagnosticSink) {
    let declarations = context.packages.declarations();
    if declarations.is_empty() {
        return;
    }
    for node in context.documentation.nodes() {
        if context.packages.declaration_at(node.range.start).is_some() {
            continue;
        }
        let start = context
            .source
            .byte_for_position(node.range.start)
            .expect("a page lies in its document");
        sink.spawn(
            &catalog::DOCUMENTATION_OUTSIDE_PACKAGE,
            Finding::new(
                context.source.remainder_of_line_range(start),
                "documentation outside every package section: no package is open here to hold \
                 it, so move it before the `endPackage` call",
            ),
        );
    }
}

/// Report `doc` called in a package section before SimpleDoc is loaded there,
/// by `beginDocumentation()` or by importing it.
fn documentation_before_begin(context: &DocumentationContext, sink: &mut DiagnosticSink) {
    let simple_doc_loads =
        collect_imported_packages_in_tree(context.root, context.syntax, context.source)
            .into_iter()
            .filter(|import| import.package.name() == SIMPLE_DOC_PACKAGE)
            .map(|import| import.effective_from)
            .collect::<Vec<_>>();
    for call in context.documentation.simple_doc_functions() {
        let Some(declaration) = context.packages.declaration_at(call.start) else {
            continue;
        };
        let loaded_before = simple_doc_loads
            .iter()
            .any(|position| declaration.contains(*position) && *position <= call.start);
        if loaded_before {
            continue;
        }
        sink.spawn(
            &catalog::DOCUMENTATION_BEFORE_BEGIN,
            Finding::new(
                *call,
                format!(
                    "`doc` comes from the SimpleDoc package, which `{}()` loads; until then it \
                     is an unset symbol here, so call `{}()` before the documentation",
                    Macaulay2Conventions::BEGIN_DOCUMENTATION,
                    Macaulay2Conventions::BEGIN_DOCUMENTATION
                ),
            ),
        );
    }
}

/// What one file says about the packages it declares, as the checks needing
/// a whole package see it.
struct PackageFile<'context> {
    /// The package sections that load no other file, and so hold all of their
    /// package's code and documentation.
    self_contained: Vec<&'context PackageDeclaration>,
    /// The symbols the file's code mentions outside of documentation, each of
    /// which Macaulay2 creates in the package the code is in.
    code_symbols: HashSet<&'context str>,
    /// Where each `beginDocumentation()` call ends, loading SimpleDoc.
    begin_documentation: Vec<Position>,
}

impl<'context> PackageFile<'context> {
    /// Builds the documentation-check context from the analyzed document and its loaded packages.
    fn read(context: &DocumentationContext<'context, 'context>) -> Self {
        let loads = FileLoad::collect(context.root, context.syntax, context.source)
            .into_iter()
            .map(|load| load.call)
            .collect::<Vec<_>>();
        let self_contained = context
            .packages
            .declarations()
            .iter()
            .filter(|declaration| !loads.iter().any(|load| declaration.contains(*load)))
            .collect();
        let in_documentation = |position: Position| {
            context
                .documentation
                .nodes()
                .iter()
                .any(|node| node.range.start <= position && position <= node.range.end)
        };
        let code_symbols = context
            .root
            .binding_references()
            .filter(|symbol| !in_documentation(context.source.position_for_node(*symbol)))
            .map(|symbol: SyntaxNode<'context>| symbol.text())
            .collect();
        let begin_documentation = context
            .root
            .descendants()
            .filter(|call| {
                call.applied_symbol_name() == Some(Macaulay2Conventions::BEGIN_DOCUMENTATION)
            })
            .map(|call| context.source.range_for_node(call).end)
            .collect();
        Self {
            self_contained,
            code_symbols,
            begin_documentation,
        }
    }

    /// The self-contained package section containing `position`.
    fn self_contained_at(&self, position: Position) -> Option<&'context PackageDeclaration> {
        self.self_contained
            .iter()
            .copied()
            .find(|declaration| declaration.contains(position))
    }
}

/// Report names documentation evaluates in a self-contained package that
/// neither the package nor the packages it loads define.
fn undefined_documentation_key(
    context: &DocumentationContext,
    file: &PackageFile,
    sink: &mut DiagnosticSink,
) {
    for referenced in context.documentation.referenced_names() {
        let range = referenced.span.range();
        let Some(declaration) = file.self_contained_at(range.start) else {
            continue;
        };
        let name = &context.source.text()[referenced.span.bytes()];
        let defined = file.code_symbols.contains(name)
            || declaration.name.name() == name
            || declaration.export_named(name).is_some()
            || context
                .knowledge
                .object_id_at(&ObjectName::new(name), range.start)
                .is_some();
        if defined {
            continue;
        }
        sink.spawn(
            &catalog::UNDEFINED_DOCUMENTATION_KEY,
            Finding::new(
                range,
                format!(
                    "`{name}` is not defined in package `{}` or in a package it loads; the \
                     documentation would create it as a new symbol, which `endPackage` rejects \
                     as unset. Fix the spelling, or define and export `{name}`",
                    declaration.name.name()
                ),
            ),
        );
    }
}

/// Report exports of a self-contained package with documentation that no page
/// documents and `undocumented` does not exempt.
fn undocumented_export(
    context: &DocumentationContext,
    file: &PackageFile,
    sink: &mut DiagnosticSink,
) {
    let documented = context
        .documentation
        .nodes()
        .iter()
        .flat_map(|node| node.keys.iter())
        .chain(context.documentation.undocumented())
        .filter_map(|key| match &key.subject {
            DocumentedSubject::Name(name) => Some(name.name()),
            DocumentedSubject::Method { .. }
            | DocumentedSubject::OptionalArgument { .. }
            | DocumentedSubject::Expression(_) => None,
        })
        .collect::<HashSet<_>>();
    for declaration in &file.self_contained {
        let has_documentation = file
            .begin_documentation
            .iter()
            .any(|position| declaration.contains(*position));
        if !has_documentation {
            continue;
        }
        for export in declaration
            .exports
            .iter()
            .filter(|export| !documented.contains(export.name.name()))
        {
            sink.spawn(
                &catalog::UNDOCUMENTED_EXPORT,
                Finding::new(
                    export.range,
                    format!(
                        "`{}` is exported but not documented, which `installPackage` warns \
                         about; document it with `Key` `{}`, or list it in `undocumented {{…}}`",
                        export.name.name(),
                        export.name.name()
                    ),
                ),
            );
        }
    }
}
