//! Versioned document snapshots that combine source text, parse tree, and
//! analysis for LSP requests.

use crate::node_metadata::{M2Node, M2Parser, M2Tree};
use m2_syn::nodes::{ExprLambda as LambdaExpression, SourceFile};
use std::sync::Mutex;
use std::thread;
use tower_lsp::lsp_types::{Position, Range as TextRange, TextDocumentContentChangeEvent};

use tree_sitter::{InputEdit, Point};

use crate::analysis::{Analysis, BindingView, FunctionInfo};
use crate::diagnostics::{self, M2Diagnostic};
use crate::documentation::{
    collect_documentation, DocumentationBlock, DocumentationIndex, DocumentationReference,
    DocumentationSnippet,
};
use crate::object_registry::{ObjectName, ObjectRegistry};
use crate::package_index::{collect_imported_packages_in_tree, PackageImport};
use crate::source::{DocumentSource, SourceNavigation};
use crate::source_package::{PackageTestBlock, SourcePackages};

/// One immutable source, syntax, and semantic-analysis snapshot served to LSP
/// requests.
#[derive(Debug)]
pub struct DocumentSnapshot {
    source: DocumentSource,
    tree: M2Tree,
    syntax: Option<SourceFile>,
    analysis: Analysis,
    diagnostics: Vec<M2Diagnostic>,
    object_registry: ObjectRegistry,
    documentation: DocumentationIndex,
    imported_packages: Vec<PackageImport>,
    packages: SourcePackages,
    package_tests: Vec<PackageTest>,
}

/// A TEST block analyzed as the session `check` runs it in: the block's code
/// alone, in a session that loaded its package. Its snapshot shares the
/// document's LSP positions, so whatever it answers for the block holds in
/// the document as is.
#[derive(Debug)]
pub struct PackageTest {
    /// The package whose exports the block sees.
    pub package: ObjectName,
    /// The packages the block's session loads along with the package.
    pub exported_packages: Vec<ObjectName>,
    /// The block's code between its `///` delimiters.
    pub body_range: TextRange,
    /// The block's code analyzed on its own.
    pub snapshot: DocumentSnapshot,
}

impl SourceNavigation for DocumentSnapshot {
    fn source(&self) -> &DocumentSource {
        &self.source
    }
}

/// The common first step of every reference / highlight / rename request: the
/// source occurrence under the cursor together with its scope-aware binding.
/// The occurrence may be a CST symbol or an indexed documentation link.
/// Resolved once per request and threaded through downstream collection.
#[derive(Debug, Clone, Copy)]
pub struct TargetSymbol<'a> {
    pub name: &'a str,
    pub range: TextRange,
    pub symbol: BindingView<'a>,
}

impl DocumentSnapshot {
    pub fn from_text(text: String, knowledge_provider: &ObjectRegistry) -> Option<Self> {
        let source = DocumentSource::new(text);
        let tree = M2Parser::new()?.parse_tree(source.text(), None)?;
        Some(Self::analyze(source, tree, knowledge_provider, Vec::new()))
    }

    /// Analyze `source`, parsed as `tree`, against the packages
    /// `knowledge_provider` has loaded plus those the source imports, then
    /// each of its TEST blocks against the same packages plus the block's,
    /// reusing the analysis of the unchanged `previous_tests`.
    fn analyze(
        source: DocumentSource,
        tree: M2Tree,
        knowledge_provider: &ObjectRegistry,
        previous_tests: Vec<PackageTest>,
    ) -> Self {
        let typed_ast = tree.typed_source_file(source.text());
        let root = tree.root(source.text());
        let imported_packages =
            collect_imported_packages_in_tree(root, typed_ast.as_ref(), &source);
        let knowledge = knowledge_provider.with_imports(&imported_packages);
        let analysis = Analysis::new_with_knowledge(root, typed_ast.as_ref(), &source, &knowledge);
        let packages = SourcePackages::collect(root, &source);
        let mut diagnostics = diagnostics::collect(
            &analysis,
            root,
            typed_ast.as_ref(),
            &source,
            &knowledge,
            &packages,
        );
        let documentation = collect_documentation(&source, root, &analysis);
        let package_tests =
            PackageTest::analyze_all(&packages, &source, knowledge_provider, previous_tests);
        diagnostics.extend(
            package_tests
                .iter()
                .flat_map(|test| test.snapshot.diagnostics.iter().cloned()),
        );
        Self {
            source,
            tree,
            syntax: typed_ast,
            analysis,
            diagnostics,
            object_registry: knowledge,
            documentation,
            imported_packages,
            packages,
            package_tests,
        }
    }

    pub fn apply_changes(
        &mut self,
        changes: &[TextDocumentContentChangeEvent],
        knowledge_provider: &ObjectRegistry,
    ) -> Option<()> {
        for change in changes {
            if let Some(range) = change.range {
                self.apply_incremental_change(range, &change.text, knowledge_provider)?;
            } else {
                let replacement = change.text.clone();
                let rebuilt = Self::from_text(replacement, knowledge_provider)?;
                *self = rebuilt;
            }
        }

        Some(())
    }

    pub fn text(&self) -> &str {
        SourceNavigation::text(self)
    }

    pub fn object_registry(&self) -> &ObjectRegistry {
        &self.object_registry
    }

    pub fn analysis(&self) -> &Analysis {
        &self.analysis
    }

    pub fn syntax(&self) -> Option<&SourceFile> {
        self.syntax.as_ref()
    }

    pub fn diagnostics(&self) -> &[M2Diagnostic] {
        &self.diagnostics
    }

    /// The packages the document imports, each from where it takes effect.
    pub fn imported_packages(&self) -> &[PackageImport] {
        &self.imported_packages
    }

    /// The packages the document declares.
    pub fn packages(&self) -> &SourcePackages {
        &self.packages
    }

    /// The document's TEST blocks, each analyzed on its own.
    pub fn package_tests(&self) -> &[PackageTest] {
        &self.package_tests
    }

    /// The TEST block whose code contains `position`, which answers for it in
    /// place of the document.
    pub fn package_test_at(&self, position: Position) -> Option<&PackageTest> {
        self.package_tests
            .iter()
            .find(|test| test.body_range.start <= position && position <= test.body_range.end)
    }

    pub fn binding_at_position(&self, position: Position) -> Option<BindingView<'_>> {
        if let Some(reference) = self.documentation_reference_at(position) {
            return self.documentation_symbol(&reference);
        }
        let node = self.reference_node_at_position(position)?;
        self.source_binding_at(node.text(), position)
    }

    pub fn source_binding_at(&self, name: &str, position: Position) -> Option<BindingView<'_>> {
        self.analysis
            .visible_source_binding_at(name, position, &self.object_registry.at(position))
    }

    pub fn future_assignment_binding_at(
        &self,
        name: &str,
        position: Position,
    ) -> Option<BindingView<'_>> {
        self.analysis
            .future_assignment_binding_at(name, position)
            .filter(|binding| {
                Analysis::source_binding_is_visible(*binding, &self.object_registry.at(position))
            })
    }

    pub fn source_symbol_at(&self, name: &str, position: Position) -> Option<BindingView<'_>> {
        self.source_binding_at(name, position)
    }

    pub fn target_symbol_at(&self, position: Position) -> Option<TargetSymbol<'_>> {
        let documentation_reference = self.documentation_reference_at(position);
        let (name, range) = if let Some(reference) = documentation_reference.as_ref() {
            (reference.name(self.text()), reference.range())
        } else {
            let node = self.reference_node_at_position(position)?;
            (node.text(), self.range_for_node(node))
        };
        let symbol = if let Some(reference) = documentation_reference {
            self.documentation_symbol(&reference)?
        } else {
            self.source_symbol_at(name, position)?
        };
        Some(TargetSymbol {
            name,
            range,
            symbol,
        })
    }

    pub fn documentation_references(&self) -> &[DocumentationReference] {
        self.documentation.references()
    }

    pub fn documentation_snippets(&self) -> &[DocumentationSnippet] {
        self.documentation.snippets()
    }

    #[cfg(test)]
    pub fn documentation_blocks(&self) -> &[DocumentationBlock] {
        self.documentation.blocks()
    }

    pub fn documentation_for_binding(
        &self,
        binding: BindingView<'_>,
    ) -> Option<&DocumentationBlock> {
        self.documentation.for_binding(binding)
    }

    pub fn documentation_for_installation(
        &self,
        installation: crate::analysis::MethodInstallationId,
    ) -> Option<&DocumentationBlock> {
        self.documentation.for_installation(installation)
    }

    pub fn documentation_reference_at(&self, position: Position) -> Option<DocumentationReference> {
        self.documentation
            .references()
            .iter()
            .find(|reference| reference.contains(position))
            .cloned()
    }

    pub fn documentation_symbol(
        &self,
        reference: &DocumentationReference,
    ) -> Option<BindingView<'_>> {
        self.analysis
            .documentation_symbol_at(reference.name(self.text()), reference.range().start)
            .filter(|binding| {
                Analysis::source_binding_is_visible(
                    *binding,
                    &self.object_registry.at(reference.range().start),
                )
            })
    }

    pub fn symbol_occurrence_at(&self, position: Position) -> Option<(&str, TextRange)> {
        if let Some(reference) = self.documentation_reference_at(position) {
            return Some((reference.name(self.text()), reference.range()));
        }
        let node = self.reference_node_at_position(position)?;
        Some((node.text(), self.range_for_node(node)))
    }

    pub fn callable_at_position(&self, position: Position) -> Option<&FunctionInfo> {
        let binding = self.binding_at_position(position)?;
        self.analysis.function_for_binding(binding)
    }

    pub fn lambda_value_ranges(&self) -> Vec<TextRange> {
        self.root_node()
            .descendants()
            .filter(|node| {
                let mut value = *node;
                while value.is_holder() {
                    let Some(inner) = value.final_value_child() else {
                        return false;
                    };
                    value = inner;
                }
                value.is::<LambdaExpression>()
            })
            .map(|node| self.range_for_node(node))
            .collect()
    }

    pub fn root_node(&self) -> M2Node<'_> {
        self.tree.root(self.text())
    }

    pub fn node_at_position_minimal(&self, position: Position) -> Option<M2Node<'_>> {
        let point = self.point_for_position(position)?;
        self.root_node().descendant_for_point_range(point, point)
    }

    /// The symbol under the cursor that refers to a binding by name; `None`
    /// on a member key, which names the symbol itself.
    pub fn reference_node_at_position(&self, position: Position) -> Option<M2Node<'_>> {
        let point = self.point_for_position(position)?;
        let root = self.root_node();
        // When the cursor sits on the boundary between the anonymous SPACE
        // application operator (which the grammar emits zero-width) and an
        // adjacent symbol — e.g. the trailing `M` in an application `(f x) M`,
        // where SPACE sits exactly at `M`'s start column — a zero-width lookup
        // lands on the operator. Widening the lookup to the character under the
        // cursor lands on the symbol; the exact-point lookup is tried first so
        // ordinary mid-token positions are unaffected.
        let next = Point::new(point.row, point.column + 1);
        let starts = [
            root.descendant_for_point_range(point, point),
            root.descendant_for_point_range(point, next),
        ];
        for start in starts.into_iter().flatten() {
            let mut node = start;
            loop {
                if node.is_symbol_like() {
                    return (!node.is_member_key()).then_some(node);
                }
                match node.parent() {
                    Some(parent) => node = parent,
                    None => break,
                }
            }
        }
        None
    }

    fn apply_incremental_change(
        &mut self,
        range: TextRange,
        replacement: &str,
        knowledge_provider: &ObjectRegistry,
    ) -> Option<()> {
        let old_bytes = self.bytes_for_range(range)?;
        let start_byte = old_bytes.start;
        let old_end_byte = old_bytes.end;
        let start_position = self.point_for_byte(start_byte);
        let old_end_position = self.point_for_byte(old_end_byte);
        let new_end_byte = start_byte + replacement.len();
        let new_end_position = advance_point(start_position, replacement);

        let edit = InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position,
            old_end_position,
            new_end_position,
        };

        let mut edited_tree = self.tree.clone();
        edited_tree.edit(&edit);
        self.source
            .replace_range(start_byte..old_end_byte, replacement);

        let tree = M2Parser::new()?.parse_tree(self.text(), Some(&edited_tree))?;
        let source = std::mem::replace(&mut self.source, DocumentSource::new(String::new()));
        let previous_tests = std::mem::take(&mut self.package_tests);
        *self = Self::analyze(source, tree, knowledge_provider, previous_tests);
        Some(())
    }
}

impl PackageTest {
    /// The TEST blocks of `packages`, declared in `source`, each analyzed
    /// alone against `knowledge_provider` plus the packages its package
    /// exports along. A block whose code, positions, and session match a
    /// test of `previous` keeps that analysis; the others are analyzed in
    /// parallel.
    fn analyze_all(
        packages: &SourcePackages,
        source: &DocumentSource,
        knowledge_provider: &ObjectRegistry,
        mut previous: Vec<PackageTest>,
    ) -> Vec<PackageTest> {
        let mut tests = Vec::with_capacity(packages.tests().len());
        let mut pending = Vec::new();
        for (ordinal, block) in packages.tests().iter().enumerate() {
            let isolated_source = block.isolated_source(source.text());
            let exported_packages = packages
                .declarations()
                .iter()
                .find(|declaration| declaration.name == block.package)
                .expect("a TEST block belongs to a package the document declares")
                .exported_packages
                .clone();
            match previous.iter().position(|test| {
                test.package == block.package
                    && test.exported_packages == exported_packages
                    && test.snapshot.text() == isolated_source
            }) {
                Some(unchanged) => tests.push((ordinal, previous.swap_remove(unchanged))),
                None => pending.push((ordinal, block, isolated_source, exported_packages)),
            }
        }

        let workers = thread::available_parallelism()
            .map_or(1, usize::from)
            .min(pending.len());
        let queue = Mutex::new(pending.into_iter());
        let analyzed = Mutex::new(Vec::new());
        thread::scope(|scope| {
            for _ in 0..workers {
                thread::Builder::new()
                    .stack_size(crate::RUNTIME_THREAD_STACK_BYTES)
                    .spawn_scoped(scope, || loop {
                        // Taken in its own statement, so the lock is released
                        // before the block is analyzed.
                        let next_block = queue.lock().expect("queue lock").next();
                        let Some((ordinal, block, isolated_source, exported_packages)) = next_block
                        else {
                            break;
                        };
                        if let Some(test) = Self::analyze(
                            block,
                            isolated_source,
                            exported_packages,
                            knowledge_provider,
                        ) {
                            analyzed
                                .lock()
                                .expect("analyzed lock")
                                .push((ordinal, test));
                        }
                    })
                    .expect("TEST analysis worker starts");
            }
        });
        tests.extend(analyzed.into_inner().expect("analyzed lock"));
        tests.sort_by_key(|(ordinal, _)| *ordinal);
        tests.into_iter().map(|(_, test)| test).collect()
    }

    /// `block`, whose isolated code is `isolated_source`, analyzed against
    /// `knowledge_provider` plus `exported_packages`.
    fn analyze(
        block: &PackageTestBlock,
        isolated_source: String,
        exported_packages: Vec<ObjectName>,
        knowledge_provider: &ObjectRegistry,
    ) -> Option<Self> {
        let session_imports = exported_packages
            .iter()
            .map(|exported| PackageImport {
                package: exported.clone(),
                effective_from: pos!(),
            })
            .collect::<Vec<_>>();
        let snapshot = DocumentSnapshot::from_text(
            isolated_source,
            &knowledge_provider.with_imports(&session_imports),
        )?;
        Some(Self {
            package: block.package.clone(),
            exported_packages,
            body_range: block.body_range,
            snapshot,
        })
    }
}

fn advance_point(start: Point, inserted_text: &str) -> Point {
    let mut row = start.row;
    let mut column = start.column;

    for byte in inserted_text.bytes() {
        if byte == b'\n' {
            row += 1;
            column = 0;
        } else {
            column += 1;
        }
    }

    Point::new(row, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_first_pass_builds_scope_and_binding_graphs() {
        let document = DocumentSnapshot::from_text(
            "outer = 1\nlocalValue := 2\nf = x -> x\nfor i from 0 to 1 do i\n".to_string(),
            &ObjectRegistry::default(),
        )
        .expect("the document should parse");

        assert_eq!(document.analysis.binding_states().count(), 5);
        assert_eq!(document.analysis.scope_count(), 5);
    }

    #[test]
    fn typed_first_pass_handles_new_installations() {
        let source = concat!(
            "M = new Type of BasicList\n",
            "new Type of BasicList from Function := (target, parent, convert) -> hashTable {}\n",
            "new M from (ZZ, ZZ) := (target, left, right) -> {left, right}\n",
            "new M := target -> {}\n",
        );
        let document = DocumentSnapshot::from_text(source.to_string(), &ObjectRegistry::default())
            .expect("the document should parse");

        assert_eq!(document.analysis.scope_count(), 4);
    }

    #[test]
    fn document_keeps_working_when_typed_syntax_rejects_incomplete_source() {
        let document =
            DocumentSnapshot::from_text("value = (\n".to_string(), &ObjectRegistry::default())
                .expect("Tree-sitter should retain an incomplete document");

        assert!(document
            .analysis
            .bindings()
            .any(|binding| binding.name.name() == "value"));
        assert_eq!(document.text(), "value = (\n");
    }

    #[test]
    fn incremental_changes_rebuild_analysis() {
        let mut document =
            DocumentSnapshot::from_text("value = 1\n".to_string(), &ObjectRegistry::default())
                .expect("the document should parse");
        document
            .apply_changes(
                &[TextDocumentContentChangeEvent {
                    range: Some(TextRange::new(Position::new(0, 8), Position::new(0, 9))),
                    range_length: None,
                    text: "\"text\"".to_string(),
                }],
                &ObjectRegistry::default(),
            )
            .expect("the incremental change should parse");

        let binding = document
            .analysis
            .binding_states()
            .find(|binding| binding.name.name() == "value")
            .expect("the edited binding should be analyzed");
        assert_eq!(
            binding.state.inferred_type,
            Some(crate::typesystem::InferredType::exact("String"))
        );
    }

    #[test]
    fn package_tests_follow_edits_inside_and_around_them() {
        let text = "newPackage \"Pkg\"\nTEST ///\nfirst = 1\n///\nendPackage \"Pkg\"\n";
        let mut document =
            DocumentSnapshot::from_text(text.to_string(), &ObjectRegistry::default())
                .expect("the document should parse");
        let edit = |line, character, text: &str| TextDocumentContentChangeEvent {
            range: Some(TextRange::new(
                Position::new(line, character),
                Position::new(line, character),
            )),
            range_length: None,
            text: text.to_string(),
        };

        document
            .apply_changes(&[edit(4, 17, "\nafter = 2")], &ObjectRegistry::default())
            .expect("an edit after the TEST block should parse");
        assert_eq!(
            document.package_tests()[0].snapshot.text().trim(),
            "first = 1"
        );

        document
            .apply_changes(&[edit(0, 0, "before = 0\n")], &ObjectRegistry::default())
            .expect("an edit before the TEST block should parse");
        assert_eq!(
            document.package_tests()[0].body_range.start,
            Position::new(2, 8)
        );

        document
            .apply_changes(&[edit(3, 0, "second = 2\n")], &ObjectRegistry::default())
            .expect("an edit inside the TEST block should parse");
        let test = &document.package_tests()[0];
        assert!(test
            .snapshot
            .analysis()
            .bindings()
            .any(|binding| binding.name.name() == "second"));
    }
}
