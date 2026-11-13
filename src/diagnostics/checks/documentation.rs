//! The documentation occasion: diagnostics about the package documentation a
//! document writes, in either format (see [`crate::package_documentation`]).
//!
//! What `document` and `SimpleDoc` reject or warn about while reading one page
//! arrives as the documentation's faults and is worded here. The remaining
//! checks need the whole file: keys documented twice, names the documentation
//! evaluates that the package never defines, exports left undocumented, and
//! documentation where Macaulay2 cannot read it.
//!
//! A package may keep code or documentation in files it loads. Definition checks
//! requiring the whole package run only for self-contained sections. Export
//! coverage is first checked locally, then replaced during publication by the
//! workspace's package-wide coverage when an index is available.

use std::collections::{HashMap, HashSet};

use m2_syn::visit::Visit;
use strum::IntoEnumIterator;

use tower_lsp::lsp_types::Position;

use super::{DiagnosticSink, DocumentationContext};
use crate::diagnostics::{catalog, Finding};
use crate::file_load::FileLoad;
use crate::macaulay2::Macaulay2Conventions;
use crate::node_metadata::ParsedCell;
use crate::object_registry::ObjectName;
use crate::package_documentation::{
    DocumentOption, DocumentTag, DocumentationFault, DocumentationFaultKind, DocumentationKey,
    DocumentationMarkupKind, DocumentedSubject, SimpleDocKeyword,
};
use crate::package_index::collect_imported_packages_in_tree;
use crate::source::SourceNavigation;
use crate::source_package::PackageDeclaration;
use crate::syntax::BindingReferences;

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
    undocumented_export(context, sink);
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
    let simple_doc_loads = collect_imported_packages_in_tree(context.syntax, context.source)
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
}

impl<'context> PackageFile<'context> {
    /// Builds the documentation-check context from the analyzed document and its loaded packages.
    fn read(context: &DocumentationContext<'context>) -> Self {
        let loads = FileLoad::collect(context.syntax, context.source)
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
        let mut references = BindingReferences::default();
        for cell in context.syntax.cells().iter().filter_map(ParsedCell::syntax) {
            references.visit_cell(cell);
        }
        let code_symbols = references
            .0
            .into_iter()
            .filter(|symbol| {
                context
                    .source
                    .position_for_syntax(*symbol)
                    .is_some_and(|position| !in_documentation(position))
            })
            .map(|symbol| symbol.text())
            .collect();
        Self {
            self_contained,
            code_symbols,
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

/// Report names documentation evaluates that no definition in reach provides:
/// not the package's code in this file, not a package it loads, not a name
/// another file loading this one defines before it. In a self-contained package
/// the documentation would create the name as a new symbol, which `endPackage`
/// rejects as unset.
fn undefined_documentation_key(
    context: &DocumentationContext,
    file: &PackageFile,
    sink: &mut DiagnosticSink,
) {
    for referenced in context.documentation.referenced_names() {
        // Markup constructors are documentation syntax, rather than target keys.
        // Preserve their references for navigation while checking the keys they consume.
        if context.documentation.markup().iter().any(|markup| {
            markup.bytes == referenced.span.bytes()
                && matches!(
                    markup.kind,
                    DocumentationMarkupKind::LinkConstructor | DocumentationMarkupKind::TextMarkup
                )
        }) {
            continue;
        }
        let range = referenced.span.range();
        let declaration = file.self_contained_at(range.start);
        let name = &context.source.text()[referenced.span.bytes()];
        let defined = file.code_symbols.contains(name)
            || context
                .packages
                .declarations()
                .iter()
                .any(|package| package.name.name() == name)
            || declaration.is_some_and(|declaration| declaration.export_named(name).is_some())
            || context
                .knowledge
                .object_id_at(&ObjectName::new(name), range.start)
                .is_some();
        if defined {
            continue;
        }
        let consequence = match declaration {
            Some(declaration) => format!(
                "`{name}` is not defined in package `{}` or in a package it loads; the \
                 documentation would create it as a new symbol, which `endPackage` rejects \
                 as unset. Fix the spelling, or define and export `{name}`",
                declaration.name.name()
            ),
            None => format!(
                "`{name}` is not defined in this file, in the files that load it, or in a \
                 package it loads; the documentation refers to nothing. Fix the spelling, or \
                 define `{name}`"
            ),
        };
        sink.spawn(
            &catalog::UNDEFINED_DOCUMENTATION_KEY,
            Finding::new(range, consequence),
        );
    }
}

/// Reports each export lacking a direct documentation key or explicit exemption
/// in its package section. Workspace publication replaces these local findings
/// with findings that also account for documentation in loaded files.
fn undocumented_export(context: &DocumentationContext, sink: &mut DiagnosticSink) {
    for declaration in context.packages.declarations() {
        let documented = context
            .documentation
            .nodes()
            .iter()
            .flat_map(|node| node.keys.iter())
            .chain(context.documentation.undocumented())
            .filter(|key| declaration.contains(key.range.start))
            .filter_map(|key| match &key.subject {
                DocumentedSubject::Name(name) => Some(name),
                _ => None,
            })
            .collect::<HashSet<_>>();
        for export in &declaration.exports {
            if !documented.contains(&export.name) {
                sink.diagnostics
                    .push(crate::diagnostics::M2Diagnostic::undocumented_export(
                        export,
                    ));
            }
        }
    }
}
