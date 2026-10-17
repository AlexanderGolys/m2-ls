//! A workspace-wide index of top-level (global) navigation facts across the
//! files the editor has open and every file they load (`load`/`needs`/`input`),
//! transitively, including loaded files the editor has not opened.
//!
//! M2's top-level symbols are global once loaded, so a name-keyed index of every
//! file's global facts is a faithful model: a name used in one file and defined
//! or implemented at the top level of another resolves across the project.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use dashmap::DashMap;
use tower_lsp::lsp_types::{Location, Range as TextRange, Url};

use crate::analysis::AssignmentFactKind;
use crate::capabilities::document_symbols::{collect_workspace_symbols, WorkspaceSourceSymbol};
use crate::document::DocumentSnapshot;
use crate::object_registry::{ObjectName, ObjectRegistry};
use crate::package_index::collect_loaded_files_in_tree;
use crate::semantic_token::{local_symbol_semantic_token, M2SemanticTokenType};

#[derive(Debug, Clone)]
struct DefLocation {
    uri: Url,
    range: TextRange,
    semantic_token_type: M2SemanticTokenType,
    is_declaration: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImplementationKind {
    Lambda,
    Method,
}

#[derive(Debug, Clone)]
struct ImplementationLocation {
    uri: Url,
    range: TextRange,
    kind: ImplementationKind,
    requires_declaration: bool,
}

pub trait WorkspaceSymbolKnowledge {
    fn matching_symbols(&self, query: &str) -> Vec<WorkspaceSourceSymbol>;
}

pub trait WorkspaceImplementationKnowledge {
    fn implementations(&self, name: &str, kind: ImplementationKind, exclude: &Url)
        -> Vec<Location>;
    fn has_method_declaration(&self, name: &str, exclude: &Url) -> bool;
}

impl<T: WorkspaceImplementationKnowledge + ?Sized> WorkspaceImplementationKnowledge for Arc<T> {
    fn implementations(
        &self,
        name: &str,
        kind: ImplementationKind,
        exclude: &Url,
    ) -> Vec<Location> {
        self.as_ref().implementations(name, kind, exclude)
    }

    fn has_method_declaration(&self, name: &str, exclude: &Url) -> bool {
        self.as_ref().has_method_declaration(name, exclude)
    }
}

impl<T: WorkspaceSymbolKnowledge + ?Sized> WorkspaceSymbolKnowledge for Arc<T> {
    fn matching_symbols(&self, query: &str) -> Vec<WorkspaceSourceSymbol> {
        self.as_ref().matching_symbols(query)
    }
}

pub trait WorkspaceDefinitionKnowledge {
    fn declarations(&self, name: &str, exclude: &Url) -> Vec<Location>;
    fn definitions(&self, name: &str, exclude: &Url) -> Vec<Location>;
    fn type_definitions(&self, name: &str, exclude: &Url) -> Vec<Location>;
    fn is_defined(&self, name: &str) -> bool;
    fn semantic_token_type(&self, name: &str, exclude: &Url) -> Option<M2SemanticTokenType>;
}

impl<T: WorkspaceDefinitionKnowledge + ?Sized> WorkspaceDefinitionKnowledge for Arc<T> {
    fn declarations(&self, name: &str, exclude: &Url) -> Vec<Location> {
        self.as_ref().declarations(name, exclude)
    }

    fn definitions(&self, name: &str, exclude: &Url) -> Vec<Location> {
        self.as_ref().definitions(name, exclude)
    }

    fn type_definitions(&self, name: &str, exclude: &Url) -> Vec<Location> {
        self.as_ref().type_definitions(name, exclude)
    }

    fn is_defined(&self, name: &str) -> bool {
        self.as_ref().is_defined(name)
    }

    fn semantic_token_type(&self, name: &str, exclude: &Url) -> Option<M2SemanticTokenType> {
        self.as_ref().semantic_token_type(name, exclude)
    }
}

/// Global navigation index, keyed by symbol name, kept in sync with edits and
/// on-disk changes. Open documents are indexed from their live text; the files
/// they load, from disk.
#[derive(Debug, Default)]
pub struct WorkspaceIndex {
    definitions_by_name: DashMap<ObjectName, Vec<DefLocation>>,
    implementations_by_name: DashMap<ObjectName, Vec<ImplementationLocation>>,
    method_declarations_by_name: DashMap<ObjectName, Vec<Url>>,
    names_by_file: DashMap<Url, Vec<ObjectName>>,
    symbols_by_file: DashMap<Url, Vec<WorkspaceSourceSymbol>>,
    /// Every indexed file, mapped to the files it loads.
    loads_by_file: DashMap<Url, Vec<Url>>,
}

impl WorkspaceIndex {
    /// Index `uri` from `text`, then every file it loads, transitively, from
    /// disk. Files already indexed keep their facts, so an open document's live
    /// text is never clobbered by its saved copy.
    pub fn index_file(&self, uri: &Url, text: &str, knowledge_provider: &ObjectRegistry) {
        let mut pending = self.index_source(uri, text, knowledge_provider);
        while let Some(loaded) = pending.pop() {
            if self.is_indexed(&loaded) {
                continue;
            }
            let Ok(path) = loaded.to_file_path() else {
                continue;
            };
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            pending.extend(self.index_source(&loaded, &text, knowledge_provider));
        }
    }

    pub fn is_indexed(&self, uri: &Url) -> bool {
        self.loads_by_file.contains_key(uri)
    }

    /// Replace the navigation facts recorded for `uri` with those parsed from
    /// `text`, returning the files it loads.
    fn index_source(&self, uri: &Url, text: &str, knowledge_provider: &ObjectRegistry) -> Vec<Url> {
        self.remove_file(uri);
        let Some(snapshot) = DocumentSnapshot::from_text(text.to_string(), knowledge_provider)
        else {
            self.loads_by_file.insert(uri.clone(), Vec::new());
            return Vec::new();
        };
        let loads = loaded_file_uris(uri, &snapshot);
        self.loads_by_file.insert(uri.clone(), loads.clone());
        let definitions = top_level_definitions(&snapshot);
        let implementations = top_level_implementations(&snapshot);
        let method_declarations = top_level_method_declarations(&snapshot);
        let symbols = collect_workspace_symbols(&snapshot, uri);
        if !symbols.is_empty() {
            self.symbols_by_file.insert(uri.clone(), symbols);
        }
        if definitions.is_empty() && implementations.is_empty() && method_declarations.is_empty() {
            return loads;
        }
        let mut names = Vec::with_capacity(
            definitions.len() + implementations.len() + method_declarations.len(),
        );
        for (name, range, semantic_token_type, is_declaration) in definitions {
            let name = ObjectName::new(name);
            self.definitions_by_name
                .entry(name.clone())
                .or_default()
                .push(DefLocation {
                    uri: uri.clone(),
                    range,
                    semantic_token_type,
                    is_declaration,
                });
            names.push(name);
        }
        for (name, range, kind, requires_declaration) in implementations {
            let name = ObjectName::new(name);
            self.implementations_by_name
                .entry(name.clone())
                .or_default()
                .push(ImplementationLocation {
                    uri: uri.clone(),
                    range,
                    kind,
                    requires_declaration,
                });
            names.push(name);
        }
        for name in method_declarations {
            let name = ObjectName::new(name);
            self.method_declarations_by_name
                .entry(name.clone())
                .or_default()
                .push(uri.clone());
            names.push(name);
        }
        names.sort();
        names.dedup();
        self.names_by_file.insert(uri.clone(), names);
        loads
    }

    pub fn remove_file(&self, uri: &Url) {
        self.loads_by_file.remove(uri);
        self.symbols_by_file.remove(uri);
        let Some((_, names)) = self.names_by_file.remove(uri) else {
            return;
        };
        for name in names {
            let now_empty = if let Some(mut locations) = self.definitions_by_name.get_mut(&name) {
                locations.retain(|location| &location.uri != uri);
                locations.is_empty()
            } else {
                false
            };
            if now_empty {
                self.definitions_by_name.remove(&name);
            }
            let implementations_empty =
                if let Some(mut locations) = self.implementations_by_name.get_mut(&name) {
                    locations.retain(|location| &location.uri != uri);
                    locations.is_empty()
                } else {
                    false
                };
            if implementations_empty {
                self.implementations_by_name.remove(&name);
            }
            let declarations_empty =
                if let Some(mut declarations) = self.method_declarations_by_name.get_mut(&name) {
                    declarations.retain(|declaration| declaration != uri);
                    declarations.is_empty()
                } else {
                    false
                };
            if declarations_empty {
                self.method_declarations_by_name.remove(&name);
            }
        }
    }

    /// Every indexed file: the open documents and the files they load.
    pub fn indexed_file_uris(&self) -> Vec<Url> {
        self.loads_by_file
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }
}

impl WorkspaceSymbolKnowledge for WorkspaceIndex {
    fn matching_symbols(&self, query: &str) -> Vec<WorkspaceSourceSymbol> {
        let query = query.to_lowercase();
        let mut symbols = self
            .symbols_by_file
            .iter()
            .flat_map(|symbols| symbols.value().clone())
            .filter(|symbol| symbol.name.name().to_lowercase().contains(&query))
            .collect::<Vec<_>>();
        symbols.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.location.uri.as_str().cmp(right.location.uri.as_str()))
                .then_with(|| left.location.range.start.cmp(&right.location.range.start))
        });
        symbols
    }
}

impl WorkspaceImplementationKnowledge for WorkspaceIndex {
    fn implementations(
        &self,
        name: &str,
        kind: ImplementationKind,
        exclude: &Url,
    ) -> Vec<Location> {
        let method_declarations = self
            .method_declarations_by_name
            .get(name)
            .map(|declarations| declarations.clone())
            .unwrap_or_default();
        let mut locations =
            self.implementations_by_name
                .get(name)
                .map_or_else(Vec::new, |locations| {
                    locations
                        .iter()
                        .filter(|location| {
                            &location.uri != exclude
                                && location.kind == kind
                                && (!location.requires_declaration
                                    || method_declarations
                                        .iter()
                                        .any(|declaration| declaration != &location.uri))
                        })
                        .map(|location| Location {
                            uri: location.uri.clone(),
                            range: location.range,
                        })
                        .collect::<Vec<_>>()
                });
        locations.sort_by(|left, right| {
            left.uri
                .as_str()
                .cmp(right.uri.as_str())
                .then_with(|| left.range.start.cmp(&right.range.start))
        });
        locations
    }

    fn has_method_declaration(&self, name: &str, exclude: &Url) -> bool {
        self.method_declarations_by_name
            .get(name)
            .is_some_and(|declarations| declarations.iter().any(|uri| uri != exclude))
    }
}

impl WorkspaceDefinitionKnowledge for WorkspaceIndex {
    fn declarations(&self, name: &str, exclude: &Url) -> Vec<Location> {
        self.locations(name, exclude, |location| location.is_declaration)
    }

    fn definitions(&self, name: &str, exclude: &Url) -> Vec<Location> {
        self.locations(name, exclude, |_| true)
    }

    fn type_definitions(&self, name: &str, exclude: &Url) -> Vec<Location> {
        self.locations(name, exclude, |location| {
            matches!(
                location.semantic_token_type,
                M2SemanticTokenType::Type | M2SemanticTokenType::Class
            )
        })
    }

    fn is_defined(&self, name: &str) -> bool {
        self.definitions_by_name.contains_key(name)
    }

    fn semantic_token_type(&self, name: &str, exclude: &Url) -> Option<M2SemanticTokenType> {
        self.definitions_by_name.get(name).and_then(|locations| {
            locations
                .iter()
                .find(|location| &location.uri != exclude)
                .map(|location| location.semantic_token_type)
        })
    }
}

impl WorkspaceIndex {
    fn locations(
        &self,
        name: &str,
        exclude: &Url,
        include: impl Fn(&DefLocation) -> bool,
    ) -> Vec<Location> {
        self.definitions_by_name
            .get(name)
            .map(|locations| {
                locations
                    .iter()
                    .filter(|location| &location.uri != exclude && include(location))
                    .map(|location| Location {
                        uri: location.uri.clone(),
                        range: location.range,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The existing files `snapshot` loads, resolved against the directory of
/// `uri` as Macaulay2 resolves them against `currentFileDirectory`.
fn loaded_file_uris(uri: &Url, snapshot: &DocumentSnapshot) -> Vec<Url> {
    let Some(directory) = uri
        .to_file_path()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    else {
        return Vec::new();
    };
    collect_loaded_files_in_tree(snapshot.root_node(), snapshot.syntax())
        .into_iter()
        .map(|loaded| directory.join(loaded))
        .filter(|path| path.is_file())
        .filter_map(|path| Url::from_file_path(path).ok())
        .collect()
}

/// Parse `text` and return its global-scope definitions as `(name, range)`,
/// where `range` is the definition site — the same range go-to-definition
/// returns for an in-file symbol.
fn top_level_definitions(
    snapshot: &DocumentSnapshot,
) -> Vec<(String, TextRange, M2SemanticTokenType, bool)> {
    let knowledge = snapshot.object_registry();
    let analysis = snapshot.analysis();
    analysis
        .binding_states()
        .filter(|binding| binding.state.scope_idx == 0)
        .map(|binding| {
            let token_type = local_symbol_semantic_token(&binding, &knowledge).token_type;
            (
                binding.name.name().to_string(),
                binding.state.span,
                token_type,
                binding.state.span == binding.range,
            )
        })
        .collect()
}

fn top_level_implementations(
    snapshot: &DocumentSnapshot,
) -> Vec<(String, TextRange, ImplementationKind, bool)> {
    let analysis = snapshot.analysis();
    let lambda_ranges = snapshot.lambda_value_ranges();
    let mut implementations = analysis
        .binding_states()
        .filter(|binding| binding.state.scope_idx == 0)
        .filter(|binding| {
            binding
                .state
                .value_range
                .is_some_and(|range| lambda_ranges.contains(&range))
        })
        .map(|binding| {
            (
                binding.name.name().to_string(),
                binding.state.span,
                ImplementationKind::Lambda,
                false,
            )
        })
        .collect::<Vec<_>>();

    implementations.extend(analysis.assignment_facts().iter().filter_map(|assignment| {
        if assignment.scope_idx != 0 {
            return None;
        }
        let AssignmentFactKind::MethodInstallation(id) = assignment.kind else {
            return None;
        };
        let installation = analysis.method_installation(id)?;
        if !installation.is_workspace_candidate() {
            return None;
        }
        Some((
            installation.method.head.name().name().to_string(),
            assignment.target_span,
            ImplementationKind::Method,
            !installation.takes_effect(),
        ))
    }));
    implementations.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.start.cmp(&right.1.start))
    });
    implementations.dedup();
    implementations
}

fn top_level_method_declarations(snapshot: &DocumentSnapshot) -> Vec<String> {
    let analysis = snapshot.analysis();
    let mut declarations = analysis
        .binding_states()
        .filter(|binding| binding.state.scope_idx == 0)
        .filter(|binding| {
            analysis
                .function_for_binding(*binding)
                .is_some_and(|function| function.is_method_function())
        })
        .map(|binding| binding.name.name().to_string())
        .collect::<Vec<_>>();
    declarations.sort();
    declarations.dedup();
    declarations
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object_registry::ObjectRegistry;

    #[test]
    fn indexes_and_looks_up_cross_file_definitions() {
        let builtins = ObjectRegistry::default();
        let index = WorkspaceIndex::default();
        let defs = Url::parse("file:///defs.m2").expect("uri");
        let main = Url::parse("file:///main.m2").expect("uri");

        index.index_file(
            &defs,
            "myHelper = x -> x + 1\nmyHelper = x -> x + 2\nGreeting = new Type of HashTable\n",
            &builtins,
        );

        let found = index.definitions("myHelper", &main);
        assert_eq!(found.len(), 2, "every reassignment should be indexed");
        assert_eq!(found[0].uri, defs);
        assert_eq!(index.declarations("myHelper", &main).len(), 1);
        assert!(
            !index.definitions("Greeting", &main).is_empty(),
            "Greeting type should be indexed"
        );
        // The current document is excluded (its live analysis is authoritative).
        assert!(index.definitions("myHelper", &defs).is_empty());

        // Re-indexing replaces; removal clears.
        index.index_file(&defs, "renamed = 1\n", &builtins);
        assert!(index.definitions("myHelper", &main).is_empty());
        assert_eq!(index.definitions("renamed", &main).len(), 1);
        index.remove_file(&defs);
        assert!(index.definitions("renamed", &main).is_empty());
    }

    #[test]
    fn imported_package_knowledge_classifies_workspace_definitions() {
        let corpus = concat!(
            "{\"kind\":\"meta\",\"default_loaded\":[\"Core\"]}\n",
            "{\"kind\":\"type\",\"name\":\"Type\",",
            "\"package\":\"$Core$Core\",\"class\":\"$Core$Type\",",
            "\"parent\":\"$Core$Thing\",\"ancestors\":[\"$Core$Thing\"]}\n",
            "{\"kind\":\"type\",\"name\":\"MethodFunction\",",
            "\"package\":\"$Core$Core\",\"class\":\"$Core$Type\",",
            "\"parent\":\"$Core$Function\",\"ancestors\":[\"$Core$Function\",\"$Core$Thing\"]}\n",
            "{\"kind\":\"methodFunction\",\"name\":\"packageType\",",
            "\"package\":\"$Pkg$Pkg\",\"class\":\"$Core$MethodFunction\",",
            "\"methods\":[{\"domain\":[\"$Core$ZZ\"],\"typicalValue\":\"$Core$Type\"}]}\n",
        );
        let knowledge = ObjectRegistry::load(corpus);
        let index = WorkspaceIndex::default();
        let definitions = Url::parse("file:///definitions.m2").unwrap();
        let reference = Url::parse("file:///reference.m2").unwrap();

        index.index_file(
            &definitions,
            "needsPackage \"Pkg\"\nGreeting = packageType 1\n",
            &knowledge,
        );

        assert_eq!(
            index.semantic_token_type("Greeting", &reference),
            Some(M2SemanticTokenType::Class)
        );
    }
}
