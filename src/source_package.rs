//! Packages declared in Macaulay2 source: each `newPackage` section with the
//! symbols it exports, the packages it exports along with them, and the TEST
//! blocks it registers, along with the code Macaulay2 runs in separate
//! sessions that load a package.
//!
//! A section runs from `newPackage "P"` to the `endPackage "P"` closing it.
//! Inside it, global assignments land in the package's private dictionary, so
//! only the names its `export`/`exportMutable` calls list are visible to the
//! sessions that load the package; after `endPackage` the file is back in the
//! `User` package and its globals are public again. A package still open at
//! the end of its file is closed by Macaulay2 from a file-exit hook, so its
//! section then runs to the end of the file.

use strum::{Display, EnumString, IntoStaticStr};
use tower_lsp::lsp_types::{Position, Range as TextRange};

use crate::node_metadata::SyntaxNode;
use crate::object_registry::ObjectName;
use crate::package_index::{declared_package_name, first_call_argument, PackageLoadingOption};
use crate::source::{ByteRange, SourceNavigation, SymbolMention};

/// The function that closes the package a `newPackage` section declares.
const PACKAGE_CLOSER: &str = "endPackage";

/// What the sessions loading a package may do with a symbol it exports,
/// decided by the function exporting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumString, IntoStaticStr, Display)]
#[non_exhaustive]
pub enum ExportAccess {
    /// `export`: the symbol is protected, so assigning it outside the
    /// package is an error.
    #[strum(serialize = "export")]
    Protected,
    /// `exportMutable`: the sessions may assign the symbol.
    #[strum(serialize = "exportMutable")]
    Mutable,
}

/// The packages one source file declares, in source order, and the TEST blocks
/// they register.
#[derive(Debug, Default)]
pub struct SourcePackages {
    declarations: Vec<PackageDeclaration>,
    tests: Vec<PackageSessionCode>,
}

/// One `newPackage` section of a source file.
#[derive(Debug, Clone)]
pub struct PackageDeclaration {
    /// The package name, which `newPackage` also binds as a global symbol.
    pub name: ObjectName,
    /// From the `newPackage` call through its closing `endPackage` call, or
    /// up to the next section or the end of the file when it is never closed.
    pub section: TextRange,
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
    /// The public name inside its export string.
    pub name_mention: SymbolMention,
    /// The name of [`Self::definition`] inside its string, a use of the
    /// package global.
    pub definition_mention: SymbolMention,
    /// Whether the sessions may assign the symbol.
    pub access: ExportAccess,
}

/// Code Macaulay2 runs in a separate session that has loaded a package, so
/// the code sees the package only through its exports: the body of a
/// `TEST ///…///` block, which `check` runs, or the examples of one
/// documentation node, which `installPackage` runs one after another in a
/// single session.
///
/// The code is a sequence of fragments of the file, such as the lines of every
/// `Example` section of a SimpleDoc node. Analyzed alone, in
/// [`Self::isolated_source`], it is that session, and whatever answers for it
/// there answers for the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSessionCode {
    /// The package the session loads; `None` for code in a file declaring no
    /// package, such as documentation or tests a package loads from its own
    /// directory.
    pub package: Option<ObjectName>,
    /// The fragments of the file the session runs, in source order.
    pub fragments: Vec<CodeFragment>,
}

/// One contiguous piece of code inside a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeFragment {
    /// The bytes of the fragment.
    pub bytes: ByteRange,
    /// The span of [`Self::bytes`].
    pub range: TextRange,
}

impl CodeFragment {
    /// The fragment covering `bytes` of `source`.
    pub fn new(bytes: ByteRange, source: &(impl SourceNavigation + ?Sized)) -> Self {
        Self {
            range: source.range_for_bytes(bytes.clone()),
            bytes,
        }
    }

    /// Whether `position` lies in the fragment, its end included.
    pub fn contains(&self, position: Position) -> bool {
        self.range.start <= position && position <= self.range.end
    }
}

impl PackageSessionCode {
    /// The fragments, out of `text`, the file they are in, placed at the same
    /// LSP positions as in the file: everything between them is blank, kept
    /// only as the line breaks and as many spaces as UTF-16 units lead up to
    /// each fragment on its first line.
    pub fn isolated_source(&self, text: &str) -> String {
        let mut isolated = String::new();
        let mut end = Position::new(0, 0);
        for fragment in &self.fragments {
            let start = fragment.range.start;
            assert!(
                end <= start,
                "the fragments of session code are ordered and disjoint"
            );
            if start.line > end.line {
                isolated.extend(std::iter::repeat_n('\n', (start.line - end.line) as usize));
                end.character = 0;
            }
            isolated.extend(std::iter::repeat_n(
                ' ',
                (start.character - end.character) as usize,
            ));
            isolated.push_str(&text[fragment.bytes.clone()]);
            end = fragment.range.end;
        }
        isolated
    }

    /// Whether `position` lies in one of the fragments.
    pub fn contains(&self, position: Position) -> bool {
        self.fragments
            .iter()
            .any(|fragment| fragment.contains(position))
    }
}

/// The package-structure calls of a source file, in source order, before they
/// are grouped into sections.
enum PackageCall<'tree> {
    Declaration(SyntaxNode<'tree>, SyntaxNode<'tree>),
    Closing(SyntaxNode<'tree>, &'tree str),
    Export(SyntaxNode<'tree>, ExportAccess),
    Test(SyntaxNode<'tree>, SyntaxNode<'tree>),
}

impl SourcePackages {
    /// The packages declared in the syntax tree under `root`.
    pub fn collect(root: SyntaxNode<'_>, source: &(impl SourceNavigation + ?Sized)) -> Self {
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
                section: source.range_for_bytes(call.start_byte()..section_end),
                exports: Vec::new(),
                exported_packages: PackageLoadingOption::Exports
                    .package_names(call)
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
                PackageCall::Export(argument, access) => {
                    let start = source.position_for_node(*argument);
                    let exports = exported_symbols(*argument, *access, source);
                    if let Some(declaration) = packages.declaration_at_mut(start) {
                        declaration.exports.extend(exports);
                    }
                }
                PackageCall::Test(clause, body) => {
                    let Some(package) =
                        packages.session_package_at(source.position_for_node(*clause))
                    else {
                        continue;
                    };
                    let Some(body) = body.string_literal_inner_bytes() else {
                        continue;
                    };
                    packages.tests.push(PackageSessionCode {
                        package,
                        fragments: vec![CodeFragment::new(body, source)],
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
    pub fn tests(&self) -> &[PackageSessionCode] {
        &self.tests
    }

    /// The package whose session runs code written at `position`, such as a
    /// TEST block or a documentation example: the package whose section
    /// contains it, or no package in a file that declares none. `None` when
    /// the file declares packages but `position` lies outside of them, where
    /// Macaulay2 has no package open to register the code with.
    pub fn session_package_at(&self, position: Position) -> Option<Option<ObjectName>> {
        if self.declarations.is_empty() {
            return Some(None);
        }
        self.declaration_at(position)
            .map(|declaration| Some(declaration.name.clone()))
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

    /// The uses of package globals its exports make by naming them.
    pub fn export_mentions(&self) -> impl Iterator<Item = &SymbolMention> {
        self.exports.iter().map(|export| &export.definition_mention)
    }

    /// The export of `name`, when the package exports it.
    pub fn export_named(&self, name: &str) -> Option<&PackageExport> {
        self.exports
            .iter()
            .find(|export| export.name.name() == name)
    }
}

impl<'tree> PackageCall<'tree> {
    fn recognize(node: SyntaxNode<'tree>) -> Option<Self> {
        if let Some(name) = declared_package_name(node) {
            return Some(Self::Declaration(node, name));
        }
        if let Some(name) = first_call_argument(node, |callee| callee == PACKAGE_CLOSER)
            .and_then(|argument| argument.string_literal_inner_text())
        {
            return Some(Self::Closing(node, name));
        }
        if let Some(access) = node
            .applied_symbol_name()
            .and_then(|callee| callee.parse::<ExportAccess>().ok())
        {
            let argument = first_call_argument(node, |callee| callee == <&str>::from(access))?;
            return Some(Self::Export(argument, access));
        }
        test_body(node).map(|body| Self::Test(node, body))
    }
}

/// The raw string a `TEST ///…///` clause runs as code.
fn test_body(clause: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
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

/// The elements of a list argument, or the argument itself when it is a
/// single element.
fn string_list(argument: SyntaxNode<'_>) -> Vec<SyntaxNode<'_>> {
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
    argument: SyntaxNode<'_>,
    access: ExportAccess,
    source: &(impl SourceNavigation + ?Sized),
) -> Vec<PackageExport> {
    let mention = |name: SyntaxNode<'_>| {
        Some(SymbolMention::new(
            source.span_for_bytes(name.string_literal_inner_bytes()?),
        ))
    };
    string_list(argument)
        .into_iter()
        .filter_map(|element| {
            if let Some(name) = element.string_literal_inner_text() {
                return Some(PackageExport {
                    name: ObjectName::new(name),
                    definition: ObjectName::new(name),
                    range: source.range_for_node(element),
                    name_mention: mention(element)?,
                    definition_mention: mention(element)?,
                    access,
                });
            }
            if !element.is_option_assignment() {
                return None;
            }
            let synonym = element.binary_parts().map(|parts| parts.left)?;
            let definition = element.binary_parts().map(|parts| parts.right)?;
            Some(PackageExport {
                name: ObjectName::new(synonym.string_literal_inner_text()?),
                definition: ObjectName::new(definition.string_literal_inner_text()?),
                range: source.range_for_node(synonym),
                name_mention: mention(synonym)?,
                definition_mention: mention(definition)?,
                access,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::SourcePackages;
    use crate::node_metadata::SyntaxParser;
    use crate::source::DocumentSource;

    fn packages(text: &str) -> SourcePackages {
        let source = DocumentSource::new(text.to_string());
        let mut parser = SyntaxParser::new().expect("parser");
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
        assert_eq!(
            package.section.end.line, 7,
            "the section ends at endPackage"
        );
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
        assert_eq!(test.fragments[0].range.start.line, 4);
    }

    #[test]
    fn an_unclosed_section_runs_to_the_next_declaration() {
        let packages = packages("newPackage \"A\"\nendPackage \"B\"\nnewPackage \"B\"\n");
        let [first, second] = packages.declarations() else {
            panic!("two packages expected");
        };
        assert_eq!(first.section.end, second.section.start);
        assert_eq!(
            second.section.end.line, 3,
            "the last section runs to the end"
        );
    }
}
