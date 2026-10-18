//! Packages declared in Macaulay2 source: each `newPackage` section with the
//! symbols it exports, the packages it exports along with them, and the TEST
//! blocks it registers.
//!
//! A section runs from `newPackage "P"` to the `endPackage "P"` closing it.
//! Inside it, global assignments land in the package's private dictionary, so
//! only the names its `export`/`exportMutable` calls list are visible to the
//! sessions that load the package; after `endPackage` the file is back in the
//! `User` package and its globals are public again. Macaulay2 1.26 closes a
//! package still open at the end of its file from a file-exit hook, but a
//! package is expected to close itself, and the server requires it to.

use tower_lsp::lsp_types::{Position, Range as TextRange};

use crate::node_metadata::M2Node;
use crate::object_registry::ObjectName;
use crate::package_index::{declared_package_name, first_call_argument};
use crate::source::{ByteRange, SourceNavigation};

/// The function that closes the package a `newPackage` section declares.
const PACKAGE_CLOSER: &str = "endPackage";

/// The functions whose arguments list the symbols a package exports;
/// `exportMutable` exports them as assignable by the importing session.
const EXPORTERS: [&str; 2] = ["export", "exportMutable"];

/// The `newPackage` option listing the packages loaded along with the package
/// into every session that loads it.
const PACKAGE_EXPORTS_OPTION: &str = "PackageExports";

/// The packages one source file declares, in source order, and the TEST blocks
/// they register.
#[derive(Debug, Default)]
pub struct SourcePackages {
    declarations: Vec<PackageDeclaration>,
    tests: Vec<PackageTestBlock>,
}

/// One `newPackage` section of a source file.
#[derive(Debug, Clone)]
pub struct PackageDeclaration {
    /// The package name, which `newPackage` also binds as a global symbol.
    pub name: ObjectName,
    /// The name string of the `newPackage` call.
    pub name_range: TextRange,
    /// From the `newPackage` call through its closing `endPackage` call, or
    /// up to the next section or the end of the file when it is never closed.
    pub section: TextRange,
    /// The `endPackage` call closing the section.
    pub closing: Option<TextRange>,
    /// The symbols the section exports, in source order.
    pub exports: Vec<PackageExport>,
    /// The packages its `PackageExports` option loads along with it.
    pub exported_packages: Vec<ObjectName>,
}

/// One symbol a package exports. `"S" => "f"` exports `S` as a synonym of the
/// package's `f`; a plain `"f"` names the symbol itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageExport {
    /// The name the importing sessions see.
    pub name: ObjectName,
    /// The package global the exported name stands for.
    pub definition: ObjectName,
    /// The string that exports the name.
    pub range: TextRange,
}

/// A `TEST ///…///` block inside a package section. `check` runs its body as
/// a separate Macaulay2 session that has loaded the package, so the body sees
/// the package only through its exports.
#[derive(Debug, Clone)]
pub struct PackageTestBlock {
    /// The package registering the test.
    pub package: ObjectName,
    /// The code between the `///` delimiters.
    pub body: ByteRange,
    /// The span of [`Self::body`].
    pub body_range: TextRange,
}

impl PackageTestBlock {
    /// The body of the block, out of `text`, the file the block is in, placed
    /// at the same LSP positions as in the file: after as many empty lines
    /// as precede it there and as many spaces as UTF-16 units precede it on
    /// its first line. Analyzed alone, it is the session `check` runs the
    /// block in, and whatever answers for it there answers for the file.
    pub fn isolated_source(&self, text: &str) -> String {
        let start = self.body_range.start;
        let body = &text[self.body.clone()];
        let mut isolated =
            String::with_capacity((start.line + start.character) as usize + body.len());
        isolated.extend(std::iter::repeat_n('\n', start.line as usize));
        isolated.extend(std::iter::repeat_n(' ', start.character as usize));
        isolated.push_str(body);
        isolated
    }
}

/// The package-structure calls of a source file, in source order, before they
/// are grouped into sections.
enum PackageCall<'tree> {
    Declaration(M2Node<'tree>, M2Node<'tree>),
    Closing(M2Node<'tree>, &'tree str),
    Export(M2Node<'tree>),
    Test(M2Node<'tree>, M2Node<'tree>),
}

impl SourcePackages {
    /// The packages declared in the syntax tree under `root`.
    pub fn collect(root: M2Node<'_>, source: &(impl SourceNavigation + ?Sized)) -> Self {
        let calls = root
            .descendants()
            .filter_map(PackageCall::recognize)
            .collect::<Vec<_>>();
        let declaration_starts = calls
            .iter()
            .filter_map(|call| match call {
                PackageCall::Declaration(call, _) => Some(call.start_byte()),
                _ => None,
            })
            .chain(std::iter::once(source.text().len()))
            .collect::<Vec<_>>();
        let mut declarations = Vec::new();
        for (call, name) in calls.iter().filter_map(|call| match call {
            PackageCall::Declaration(call, name) => Some((*call, *name)),
            _ => None,
        }) {
            let next_section_start = declaration_starts
                .iter()
                .copied()
                .find(|start| *start > call.start_byte())
                .expect("the end of the file follows every section");
            let package_name = name
                .string_literal_inner_text()
                .expect("a declared package name is a string literal");
            let closing = calls.iter().find_map(|closing| match closing {
                PackageCall::Closing(closing, closed)
                    if *closed == package_name
                        && closing.start_byte() > call.start_byte()
                        && closing.start_byte() < next_section_start =>
                {
                    Some(*closing)
                }
                _ => None,
            });
            let section_end = closing.map_or(next_section_start, |closing| closing.end_byte());
            declarations.push(PackageDeclaration {
                name: ObjectName::new(package_name),
                name_range: source.range_for_node(name),
                section: source.range_for_bytes(call.start_byte()..section_end),
                closing: closing.map(|closing| source.range_for_node(closing)),
                exports: Vec::new(),
                exported_packages: package_exports_option(call)
                    .map(string_list)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|package| package.string_literal_inner_text())
                    .map(ObjectName::new)
                    .collect(),
            });
        }

        let mut packages = Self {
            declarations,
            tests: Vec::new(),
        };
        for call in &calls {
            match call {
                PackageCall::Export(argument) => {
                    let start = source.position_for_node(*argument);
                    let exports = exported_symbols(*argument, source);
                    if let Some(declaration) = packages.declaration_at_mut(start) {
                        declaration.exports.extend(exports);
                    }
                }
                PackageCall::Test(clause, body) => {
                    let Some(package) = packages.declaration_at(source.position_for_node(*clause))
                    else {
                        continue;
                    };
                    let Some(body) = body.string_literal_inner_bytes() else {
                        continue;
                    };
                    packages.tests.push(PackageTestBlock {
                        package: package.name.clone(),
                        body_range: source.range_for_bytes(body.clone()),
                        body,
                    });
                }
                PackageCall::Declaration(..) | PackageCall::Closing(..) => {}
            }
        }
        packages
    }

    /// Every package the file declares, in source order.
    pub fn declarations(&self) -> &[PackageDeclaration] {
        &self.declarations
    }

    /// The TEST blocks the file's packages register, in source order.
    pub fn tests(&self) -> &[PackageTestBlock] {
        &self.tests
    }

    /// The package whose section contains `position`: the one global
    /// definitions made there belong to.
    pub fn declaration_at(&self, position: Position) -> Option<&PackageDeclaration> {
        self.declarations
            .iter()
            .find(|declaration| declaration.contains(position))
    }

    fn declaration_at_mut(&mut self, position: Position) -> Option<&mut PackageDeclaration> {
        self.declarations
            .iter_mut()
            .find(|declaration| declaration.contains(position))
    }
}

impl PackageDeclaration {
    /// Whether `position` lies in the section.
    pub fn contains(&self, position: Position) -> bool {
        self.section.start <= position && position <= self.section.end
    }

    /// The export of `name`, when the package exports it.
    pub fn export_named(&self, name: &str) -> Option<&PackageExport> {
        self.exports
            .iter()
            .find(|export| export.name.name() == name)
    }
}

impl<'tree> PackageCall<'tree> {
    fn recognize(node: M2Node<'tree>) -> Option<Self> {
        if let Some(name) = declared_package_name(node) {
            return Some(Self::Declaration(node, name));
        }
        if let Some(name) = first_call_argument(node, |callee| callee == PACKAGE_CLOSER)
            .and_then(|argument| argument.string_literal_inner_text())
        {
            return Some(Self::Closing(node, name));
        }
        if let Some(argument) = first_call_argument(node, |callee| EXPORTERS.contains(&callee)) {
            return Some(Self::Export(argument));
        }
        test_body(node).map(|body| Self::Test(node, body))
    }
}

/// The raw string a `TEST ///…///` clause runs as code.
fn test_body(clause: M2Node<'_>) -> Option<M2Node<'_>> {
    if !clause.is_debug_expr()
        || !clause
            .children()
            .any(|child| child.is::<m2_syn::Token![TEST]>())
    {
        return None;
    }
    clause
        .named_children()
        .find(|child| child.is::<m2_syn::nodes::RawStringLiteral>())
}

/// The value of the `PackageExports => …` option of a `newPackage` call.
fn package_exports_option(call: M2Node<'_>) -> Option<M2Node<'_>> {
    let arguments = call.child_by_field_name("right")?;
    let options = arguments.collection_elements().collect::<Vec<_>>();
    options.into_iter().find_map(|option| {
        let key = option.child_by_field_name("left")?;
        (option.is_option_assignment() && key.text() == PACKAGE_EXPORTS_OPTION)
            .then(|| option.child_by_field_name("right"))?
    })
}

/// The elements of a list argument, or the argument itself when it is a
/// single element.
fn string_list(argument: M2Node<'_>) -> Vec<M2Node<'_>> {
    if argument.is_collection_expression() {
        argument.collection_elements().collect()
    } else {
        vec![argument]
    }
}

/// The symbols an `export`/`exportMutable` argument lists: strings, and
/// `"synonym" => "name"` options. Anything else is computed when the package
/// loads and cannot be known here.
fn exported_symbols(
    argument: M2Node<'_>,
    source: &(impl SourceNavigation + ?Sized),
) -> Vec<PackageExport> {
    string_list(argument)
        .into_iter()
        .filter_map(|element| {
            if let Some(name) = element.string_literal_inner_text() {
                return Some(PackageExport {
                    name: ObjectName::new(name),
                    definition: ObjectName::new(name),
                    range: source.range_for_node(element),
                });
            }
            if !element.is_option_assignment() {
                return None;
            }
            let synonym = element.child_by_field_name("left")?;
            let definition = element.child_by_field_name("right")?;
            Some(PackageExport {
                name: ObjectName::new(synonym.string_literal_inner_text()?),
                definition: ObjectName::new(definition.string_literal_inner_text()?),
                range: source.range_for_node(synonym),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::SourcePackages;
    use crate::node_metadata::M2Parser;
    use crate::source::DocumentSource;

    fn packages(text: &str) -> SourcePackages {
        let source = DocumentSource::new(text.to_string());
        let mut parser = M2Parser::new().expect("parser");
        let root = parser.parse(text).expect("parse");
        SourcePackages::collect(root, &source)
    }

    #[test]
    fn reads_sections_exports_and_tests() {
        let packages = packages(concat!(
            "newPackage(\"Pkg\", Version => \"0.1\", PackageExports => {\"Graphs\"})\n",
            "export {\"pub\", \"Syn\" => \"pub\"}\n",
            "exportMutable \"counter\"\n",
            "pub = x -> x\n",
            "TEST ///\nassert(pub 1 == 1)\n///\n",
            "endPackage \"Pkg\"\n",
            "TEST ///after///\n",
        ));
        let [package] = packages.declarations() else {
            panic!("one package expected");
        };
        assert_eq!(package.name.name(), "Pkg");
        assert!(package.closing.is_some());
        assert_eq!(
            package
                .exports
                .iter()
                .map(|export| (export.name.name(), export.definition.name()))
                .collect::<Vec<_>>(),
            [("pub", "pub"), ("Syn", "pub"), ("counter", "counter")]
        );
        assert_eq!(package.exported_packages.len(), 1);
        let [test] = packages.tests() else {
            panic!("only the TEST inside the section is registered");
        };
        assert_eq!(test.body_range.start.line, 4);
    }

    #[test]
    fn an_unclosed_section_runs_to_the_next_declaration() {
        let packages = packages("newPackage \"A\"\nendPackage \"B\"\nnewPackage \"B\"\n");
        let [first, second] = packages.declarations() else {
            panic!("two packages expected");
        };
        assert!(first.closing.is_none());
        assert!(second.closing.is_none());
        assert_eq!(first.section.end, second.section.start);
    }
}
