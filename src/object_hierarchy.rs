//! Object, namespace, class, and inheritance relations shared by every origin.
//!
//! A package owns a namespace; a type is an object with a runtime class and
//! an inheritance parent. Namespace membership, `classOf`, and `parentOf` are
//! independent edges. Only `parentOf` participates in subtype checks.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write;

use serde::Serialize;
use tower_lsp::lsp_types::{Location, Position, Url};

use crate::builtin_index::Record;
use crate::document::DocumentSnapshot;
use crate::macaulay2::Macaulay2Conventions;
use crate::object_registry::{
    ObjectId, ObjectKnowledge, ObjectRegistry, TypeData, TypeId, TypeStore,
};

/// Borrowed object relations combining a document's facts with its environment.
/// Neither traversal nor subtype logic distinguishes where an object originated.
pub struct ObjectRelations<'a, K: ?Sized> {
    /// Parent edges retained for source-created type objects.
    source_types: &'a HashMap<TypeId, TypeData>,
    /// Runtime classes captured when source values were assigned.
    source_classes: &'a HashMap<ObjectId, TypeId>,
    /// Objects already available in the surrounding environment.
    environment: &'a K,
}

impl<'a, K: ObjectKnowledge + ?Sized> ObjectRelations<'a, K> {
    /// Borrows the source edges and surrounding object environment.
    pub fn new(
        source_types: &'a HashMap<TypeId, TypeData>,
        source_classes: &'a HashMap<ObjectId, TypeId>,
        environment: &'a K,
    ) -> Self {
        Self {
            source_types,
            source_classes,
            environment,
        }
    }

    /// Runtime class of any object, independently of its inheritance parent.
    pub fn class_of(&self, object: &ObjectId) -> Option<TypeId> {
        self.source_classes
            .get(object)
            .cloned()
            .or_else(|| self.environment.class_type_id(object))
    }
}

impl<K: ObjectKnowledge + ?Sized> TypeStore for ObjectRelations<'_, K> {
    fn parent_type_id(&self, type_id: &TypeId) -> Option<TypeId> {
        self.source_types
            .get(type_id)
            .and_then(|data| data.parent.clone())
            .or_else(|| self.environment.parent_type_id(type_id))
    }

    fn has_strict_subtype_id(&self, type_id: &TypeId) -> bool {
        self.source_types
            .iter()
            .any(|(candidate, data)| candidate != type_id && data.parent.as_ref() == Some(type_id))
            || self.environment.has_strict_subtype_id(type_id)
    }
}

/// Identity of a dictionary, separate from the object that may own it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "kind", content = "owner", rename_all = "camelCase")]
#[non_exhaustive]
pub enum NamespaceId {
    /// Dictionary owned by a package object.
    Package(ObjectId),
    /// Private dictionary owned by a package object.
    PackagePrivate(ObjectId),
    /// Dictionary binding package names to package objects.
    Packages,
    /// A lexical source scope, distinct from package dictionaries.
    Scope {
        /// Session owning this scope.
        document: Url,
        /// Index in the retained analysis's scope tree.
        index: usize,
    },
    /// A document session's user dictionary.
    Document(Url),
}

/// One namespace with its owning package, if any, and object memberships.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Namespace {
    /// Dictionary identity; this is not a type identity or an inheritance edge.
    pub id: NamespaceId,
    /// Human-readable package or session name.
    pub name: String,
    /// Objects named by this dictionary, including type and package objects.
    pub members: BTreeSet<ObjectId>,
    /// Names in this dictionary, including exported aliases of the same object.
    pub bindings: BTreeMap<String, ObjectId>,
    /// Enclosing lexical dictionary, when this is a source scope.
    pub enclosing: Option<NamespaceId>,
    /// Package owning this dictionary; the package-name registry has no package owner.
    pub owner_package: Option<ObjectId>,
    /// Qualified Macaulay2 names; internal object ids are not qualified aliases.
    pub qualified_bindings: BTreeMap<String, ObjectId>,
}

impl Namespace {
    /// Binds a name and retains only objects that still have a name in this dictionary.
    pub fn bind(&mut self, name: String, object: ObjectId) {
        if let Some(qualified) = self.qualified_name(&name) {
            self.qualified_bindings.insert(qualified, object.clone());
        }
        let previous = self.bindings.insert(name, object.clone());
        self.members.insert(object);
        if let Some(previous) = previous {
            if !self.bindings.values().any(|object| object == &previous) {
                self.members.remove(&previous);
            }
        }
    }
    /// Source-qualified spelling when this dictionary has a public qualifier.
    /// User's package object is bound by its bare name and receives no fabricated alias.
    pub fn qualified_name(&self, name: &str) -> Option<String> {
        let prefix = match &self.id {
            NamespaceId::Package(_) => self.name.as_str(),
            NamespaceId::Document(_) => Macaulay2Conventions::USER_PACKAGE,
            NamespaceId::Packages if name != Macaulay2Conventions::USER_PACKAGE => {
                Macaulay2Conventions::PACKAGE_NAMESPACE
            }
            _ => return None,
        };
        Some(format!("{prefix}${name}"))
    }

    /// Writes the dictionary and its named bindings at the requested tree depth.
    pub fn render(&self, text: &mut String, indent: &str) {
        let _ = writeln!(text, "{indent}{}", self.name);
        if let Some(enclosing) = &self.enclosing {
            let _ = writeln!(text, "{indent}  enclosing: {enclosing:?}");
        }
        for (name, id) in &self.bindings {
            let qualified = self.qualified_name(name).unwrap_or_else(|| name.clone());
            let _ = writeln!(text, "{indent}  {qualified} [{}]", id.name());
        }
    }
}

/// Common semantic shape of a named object, including types and packages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HierarchyObject {
    /// Canonical value identity, independent of its display name.
    pub id: ObjectId,
    /// Source-visible name of the object.
    pub name: String,
    /// Runtime class; never substituted for an inheritance parent.
    pub class_of: Option<TypeId>,
    /// Inheritance parent, present only for objects known to be types.
    pub parent_of: Option<TypeId>,
    /// Validated type identity when the object itself denotes a type.
    pub type_id: Option<TypeId>,
    /// Owning dictionary, independent of both type relations.
    pub namespace: NamespaceId,
    /// Associated defining package, independently of the namespace binding its name.
    pub package: Option<ObjectId>,
    /// Declaration location, when available from source.
    pub location: Option<Location>,
    /// Lexical scope for source bindings; absent for environment records.
    pub scope: Option<usize>,
    /// Public dependency names for package objects; `None` means unknown metadata.
    pub package_exports: Option<Vec<String>>,
}

/// A package export binds a public name to an object from its private dictionary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamespaceExport {
    /// Package whose public dictionary receives the binding.
    pub package: ObjectId,
    /// Name visible to importing code.
    pub name: String,
    /// Name looked up in the package's private dictionary.
    pub definition: String,
}

/// Autoloading a package's public API preserves the dependency's namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageExport {
    /// Package forwarding a public dependency.
    pub package: ObjectId,
    /// Dependency name, retained even when it is not yet indexed.
    pub name: String,
    /// Dependency object identity, when known unambiguously.
    pub dependency: Option<ObjectId>,
}

/// A materialized view of the same relations consumed by typechecking.
///
/// Nodes from source and the environment have the same representation. Cycles,
/// missing edges, and same-named objects retain their identities in the report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectHierarchy {
    /// Objects keyed by canonical identity, including types and package owners.
    pub objects: BTreeMap<ObjectId, HierarchyObject>,
    /// Package and document dictionaries with explicit memberships.
    pub namespaces: Vec<Namespace>,
    /// Export relations retained even when the defining file has not been indexed.
    pub exports: Vec<NamespaceExport>,
    /// Public package dependencies, separate from dictionary ownership and type edges.
    pub package_exports: Vec<PackageExport>,
}

impl ObjectHierarchy {
    /// Gets or creates a namespace, including dictionaries that have no bindings yet.
    pub fn namespace_mut(&mut self, id: NamespaceId, name: String) -> &mut Namespace {
        let index = self
            .namespaces
            .iter()
            .position(|namespace| namespace.id == id)
            .unwrap_or_else(|| {
                self.namespaces.push(Namespace {
                    owner_package: match &id {
                        NamespaceId::Package(owner) | NamespaceId::PackagePrivate(owner) => {
                            Some(owner.clone())
                        }
                        NamespaceId::Document(_) => Some(ObjectId::session_package(
                            Macaulay2Conventions::USER_PACKAGE,
                        )),
                        _ => None,
                    },
                    qualified_bindings: BTreeMap::new(),
                    id,
                    name,
                    members: BTreeSet::new(),
                    bindings: BTreeMap::new(),
                    enclosing: None,
                });
                self.namespaces.len() - 1
            });
        &mut self.namespaces[index]
    }

    /// Adds or replaces an object and records its namespace membership.
    pub fn insert(&mut self, mut object: HierarchyObject, namespace_name: String) {
        if let Some(previous) = self.objects.get(&object.id) {
            object.class_of = object.class_of.or_else(|| previous.class_of.clone());
            object.parent_of = object.parent_of.or_else(|| previous.parent_of.clone());
            object.type_id = object.type_id.or_else(|| previous.type_id.clone());
            object.package = object.package.or_else(|| previous.package.clone());
            object.package_exports = object
                .package_exports
                .or_else(|| previous.package_exports.clone());
        }
        self.bind(
            object.namespace.clone(),
            namespace_name,
            object.name.clone(),
            object.id.clone(),
        );
        self.objects.insert(object.id.clone(), object);
    }

    /// Binds another name to an existing object without duplicating that object.
    pub fn bind(
        &mut self,
        id: NamespaceId,
        namespace_name: String,
        name: String,
        object: ObjectId,
    ) {
        self.namespace_mut(id, namespace_name).bind(name, object);
    }

    /// Combines facts without collapsing objects that happen to share a name.
    pub fn merge(&mut self, other: Self) {
        self.exports.extend(other.exports);
        let namespaces = other.namespaces;
        let names = namespaces
            .iter()
            .map(|namespace| (namespace.id.clone(), namespace.name.clone()))
            .collect::<BTreeMap<_, _>>();
        for object in other.objects.into_values() {
            let name = names.get(&object.namespace).cloned().unwrap_or_default();
            self.insert(object, name);
        }
        for namespace in namespaces {
            self.namespace_mut(namespace.id.clone(), namespace.name.clone());
            for (name, object) in namespace.bindings {
                self.bind(namespace.id.clone(), namespace.name.clone(), name, object);
            }
            if let Some(current) = self
                .namespaces
                .iter_mut()
                .find(|current| current.id == namespace.id)
            {
                current.enclosing = namespace.enclosing;
            }
        }
        self.namespaces.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.cmp(&right.id))
        });
    }

    /// Adds environment records and the classes and parents they reference.
    pub fn include_environment(&mut self, registry: &ObjectRegistry, position: Position) {
        let mut pending = registry
            .records_by_precedence_at(position)
            .map(|record| record.id.clone())
            .collect::<Vec<_>>();
        let mut seen = HashSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id.clone()) || self.objects.contains_key(&id) {
                continue;
            }
            let Some(record) = registry.object(&id) else {
                continue;
            };
            let object = Self::record_object(record, registry);
            pending.extend(
                object
                    .class_of
                    .iter()
                    .chain(&object.parent_of)
                    .map(|type_id| type_id.object().clone()),
            );
            pending.push(record.package.clone());
            let namespace_name = registry
                .package_name(&record.package)
                .unwrap_or(record.package.name())
                .to_string();
            let namespace_name = if object.namespace == NamespaceId::Packages {
                Macaulay2Conventions::PACKAGE_DICTIONARY.to_string()
            } else {
                namespace_name
            };
            let namespace = object.namespace.clone();
            self.insert(object, namespace_name.clone());
            let prefix = match &namespace {
                NamespaceId::Packages => Macaulay2Conventions::PACKAGE_NAMESPACE,
                _ => registry
                    .package_name(&record.package)
                    .unwrap_or(record.package.name()),
            };
            for name in record.lookup_names() {
                let Some((owner, unqualified)) = name.name().split_once('$') else {
                    continue;
                };
                if owner == prefix {
                    self.bind(
                        namespace.clone(),
                        namespace_name.clone(),
                        unqualified.to_string(),
                        record.id.clone(),
                    );
                }
            }
        }
        self.namespaces.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.cmp(&right.id))
        });
    }

    /// Converts an environment object without inferring one relation from another.
    fn record_object(record: &Record, registry: &ObjectRegistry) -> HierarchyObject {
        HierarchyObject {
            id: record.id.clone(),
            name: record.name.name().to_string(),
            class_of: record.class_id.clone(),
            parent_of: record.type_info().and_then(|data| data.parent.clone()),
            type_id: registry.type_id(&record.id),
            namespace: if record.id == record.package {
                NamespaceId::Packages
            } else {
                NamespaceId::Package(record.package.clone())
            },
            package: Some(record.package.clone()),
            location: None,
            scope: None,
            package_exports: record
                .package_exports
                .as_ref()
                .map(|names| names.iter().map(|name| name.name().to_string()).collect()),
        }
    }

    /// Retains the implicit User package without inventing a `User$User` alias.
    pub fn include_user_package(&mut self, registry: &ObjectRegistry) {
        let name = Macaulay2Conventions::USER_PACKAGE;
        let id = registry
            .catalog_package_id(&crate::object_registry::ObjectName::new(name))
            .cloned()
            .unwrap_or_else(|| ObjectId::session_package(name));
        if self.objects.contains_key(&id) {
            return;
        }
        self.insert(
            HierarchyObject {
                id,
                name: name.to_string(),
                class_of: registry
                    .catalog_package_id(&crate::object_registry::ObjectName::new(
                        Macaulay2Conventions::CORE,
                    ))
                    .and_then(|core| registry.object(core))
                    .and_then(|core| core.class_id.clone()),
                parent_of: None,
                type_id: None,
                namespace: NamespaceId::Packages,
                package: None,
                location: None,
                scope: None,
                package_exports: Some(Vec::new()),
            },
            Macaulay2Conventions::PACKAGE_DICTIONARY.to_string(),
        );
    }

    /// Captures source objects using the analysis's canonical class and parent edges.
    pub fn source_document(document: &DocumentSnapshot, uri: &Url) -> Self {
        let analysis = document.analysis();
        let registry = document.object_registry();
        let relations = analysis.object_relations(registry);
        let mut result = Self::default();
        result.include_user_package(registry);
        let package_objects = document
            .packages()
            .declarations()
            .iter()
            .filter_map(|package| {
                let binding = analysis.binding_states().find(|binding| {
                    binding.name == package.name
                        && binding.state.presentation_kind
                            == tower_lsp::lsp_types::SymbolKind::NAMESPACE
                        && package.contains(binding.state.span.start)
                })?;
                Some((package.section.start, analysis.binding_object_id(binding)))
            })
            .collect::<BTreeMap<_, _>>();
        for package in document.packages().declarations() {
            if let Some(owner) = package_objects.get(&package.section.start) {
                result.namespace_mut(
                    NamespaceId::Package(owner.clone()),
                    package.name.name().to_string(),
                );
                result.namespace_mut(
                    NamespaceId::PackagePrivate(owner.clone()),
                    format!("{} (private)", package.name.name()),
                );
            }
        }
        for (index, range, parent) in analysis.scope_ranges().filter(|(index, _, _)| *index != 0) {
            let (outer, outer_name) =
                Self::source_namespace(document, uri, range.start, &package_objects);
            result.namespace_mut(outer.clone(), outer_name);
            let enclosing = parent
                .filter(|parent| *parent != 0)
                .map(|index| NamespaceId::Scope {
                    document: uri.clone(),
                    index,
                })
                .unwrap_or(outer);
            result
                .namespace_mut(
                    NamespaceId::Scope {
                        document: uri.clone(),
                        index,
                    },
                    format!("{uri} scope {index}"),
                )
                .enclosing = Some(enclosing);
        }
        for binding in analysis.binding_states() {
            let id = analysis.binding_object_id(binding);
            let dictionary = analysis.dictionary_scope(binding.scope_idx).unwrap_or(0);
            let (namespace, name) = if binding.state.presentation_kind
                == tower_lsp::lsp_types::SymbolKind::NAMESPACE
            {
                (
                    NamespaceId::Packages,
                    Macaulay2Conventions::PACKAGE_DICTIONARY.to_string(),
                )
            } else if dictionary != 0 {
                (
                    NamespaceId::Scope {
                        document: uri.clone(),
                        index: dictionary,
                    },
                    format!("{uri} scope {dictionary}"),
                )
            } else {
                Self::source_namespace(document, uri, binding.state.span.start, &package_objects)
            };
            if result.objects.contains_key(&id) || registry.object(&id).is_some() {
                result.bind(namespace, name, binding.name.name().to_string(), id);
                continue;
            }
            result.insert(
                HierarchyObject {
                    id: id.clone(),
                    name: binding.name.name().to_string(),
                    class_of: relations.class_of(&id),
                    parent_of: binding
                        .state
                        .source_type
                        .as_ref()
                        .and_then(|type_id| relations.parent_type_id(type_id)),
                    type_id: binding.state.source_type.clone(),
                    namespace,
                    package: None,
                    location: Some(Location::new(uri.clone(), binding.state.span)),
                    scope: Some(binding.scope_idx),
                    package_exports: None,
                },
                name,
            );
        }
        for package in document.packages().declarations() {
            let Some(owner) = package_objects.get(&package.section.start) else {
                continue;
            };
            if let Some(object) = result.objects.get_mut(owner) {
                object.package_exports = Some(
                    package
                        .exported_packages
                        .iter()
                        .map(|name| name.name().to_string())
                        .collect(),
                );
            }
            for export in &package.exports {
                result.exports.push(NamespaceExport {
                    package: owner.clone(),
                    name: export.name.name().to_string(),
                    definition: export.definition.name().to_string(),
                });
            }
        }
        result.resolve_exports();
        result
    }

    /// Owning package dictionary or user dictionary at a source position.
    fn source_namespace(
        document: &DocumentSnapshot,
        uri: &Url,
        position: Position,
        packages: &BTreeMap<Position, ObjectId>,
    ) -> (NamespaceId, String) {
        document
            .packages()
            .declarations()
            .iter()
            .find(|package| package.contains(position))
            .and_then(|package| {
                packages.get(&package.section.start).map(|owner| {
                    (
                        NamespaceId::PackagePrivate(owner.clone()),
                        format!("{} (private)", package.name.name()),
                    )
                })
            })
            .unwrap_or_else(|| {
                (
                    NamespaceId::Document(uri.clone()),
                    format!("{} @ {uri}", Macaulay2Conventions::USER_PACKAGE),
                )
            })
    }

    /// Resolves public names using the same object identities as private names.
    pub fn resolve_exports(&mut self) {
        for export in self.exports.clone() {
            let object = self
                .namespaces
                .iter()
                .find(|namespace| {
                    namespace.id == NamespaceId::PackagePrivate(export.package.clone())
                })
                .and_then(|namespace| namespace.bindings.get(&export.definition))
                .cloned();
            if let Some(object) = object {
                let package_name = self
                    .objects
                    .get(&export.package)
                    .map(|package| package.name.clone())
                    .unwrap_or_default();
                self.bind(
                    NamespaceId::Package(export.package),
                    package_name,
                    export.name,
                    object,
                );
            }
        }
    }

    /// Attaches a loaded file's globals to the package dictionary it executes in.
    pub fn attach_to_package(&mut self, uri: &Url, package: &str) {
        let owners = self
            .namespaces
            .iter()
            .filter(|namespace| namespace.id == NamespaceId::Packages)
            .filter_map(|namespace| namespace.bindings.get(package))
            .cloned()
            .collect::<Vec<_>>();
        let [owner] = owners.as_slice() else {
            return;
        };
        let target = NamespaceId::PackagePrivate(owner.clone());
        let source = NamespaceId::Document(uri.clone());
        let bindings = self
            .namespaces
            .iter()
            .find(|namespace| namespace.id == source)
            .map(|namespace| namespace.bindings.clone())
            .unwrap_or_default();
        for (name, object) in bindings {
            if let Some(value) = self
                .objects
                .get_mut(&object)
                .filter(|value| value.namespace == source)
            {
                value.namespace = target.clone();
            }
            self.bind(target.clone(), format!("{package} (private)"), name, object);
        }
        for namespace in &mut self.namespaces {
            if namespace.enclosing.as_ref() == Some(&source) {
                namespace.enclosing = Some(target.clone());
            }
        }
        self.namespaces.retain(|namespace| namespace.id != source);
    }

    /// Captures known package associations without confusing a package's name namespace with its owner.
    /// Package objects are associated with themselves; unowned lexical dictionaries stay unknown.
    pub fn resolve_packages(&mut self) {
        for object in self.objects.values_mut() {
            if object.package.is_some() {
                continue;
            }
            object.package = if object.namespace == NamespaceId::Packages {
                Some(object.id.clone())
            } else {
                self.namespaces
                    .iter()
                    .find(|namespace| namespace.id == object.namespace)
                    .and_then(|namespace| namespace.owner_package.clone())
            };
        }
    }

    /// Links public dependency names without moving their objects to another namespace.
    pub fn resolve_package_exports(&mut self) {
        self.package_exports = self
            .objects
            .values()
            .flat_map(|object| {
                object.package_exports.iter().flatten().map(|name| {
                    let candidates = self
                        .objects
                        .values()
                        .filter(|candidate| {
                            candidate.namespace == NamespaceId::Packages && candidate.name == *name
                        })
                        .collect::<Vec<_>>();
                    let dependency = match candidates.as_slice() {
                        [candidate] => Some(candidate.id.clone()),
                        _ => None,
                    };
                    PackageExport {
                        package: object.id.clone(),
                        name: name.clone(),
                        dependency,
                    }
                })
            })
            .collect();
    }

    /// Public dictionaries loaded with a package, following re-export edges safely.
    pub fn public_namespaces(&self, package: &ObjectId) -> Vec<&Namespace> {
        let mut pending = vec![package.clone()];
        let mut visited = HashSet::new();
        let mut namespaces = Vec::new();
        while let Some(package) = pending.pop() {
            if !visited.insert(package.clone()) {
                continue;
            }
            if let Some(namespace) = self
                .namespaces
                .iter()
                .find(|namespace| namespace.id == NamespaceId::Package(package.clone()))
            {
                namespaces.push(namespace);
            }
            pending.extend(
                self.package_exports
                    .iter()
                    .filter(|export| export.package == package)
                    .filter_map(|export| export.dependency.clone()),
            );
        }
        namespaces
    }

    /// Resolves qualified names through dictionary bindings, retaining ambiguous identities.
    /// User dictionaries belong to their document sessions; public package qualifiers are shared.
    pub fn resolve_qualified(&self, name: &str, document: &Url) -> Vec<ObjectId> {
        self.namespaces.iter().filter(|namespace| !matches!(&namespace.id, NamespaceId::Document(owner) if owner != document))
            .filter_map(|namespace| namespace.qualified_bindings.get(name).cloned())
            .collect::<BTreeSet<_>>().into_iter().collect()
    }

    /// Runtime class of any retained object, irrespective of its origin or kind.
    pub fn class_of(&self, object: &ObjectId) -> Option<&TypeId> {
        self.objects.get(object)?.class_of.as_ref()
    }

    /// Direct children in the inheritance relation; classes are not children here.
    pub fn subtypes(&self, parent: &TypeId) -> Vec<&HierarchyObject> {
        self.objects
            .values()
            .filter(|object| {
                object.type_id.is_some()
                    && object.id != *parent.object()
                    && object.parent_of.as_ref() == Some(parent)
            })
            .collect()
    }

    /// A deterministic readable package tree and separately labelled type relations.
    pub fn render(&self) -> String {
        let mut text = String::from("Packages and namespaces (ownership, not inheritance)\n");
        let mut namespaces = self.namespaces.iter().collect::<Vec<_>>();
        namespaces.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut rendered = HashSet::new();
        if let Some(packages) = namespaces
            .iter()
            .find(|namespace| namespace.id == NamespaceId::Packages)
        {
            let _ = writeln!(text, "{}", packages.name);
            rendered.insert(packages.id.clone());
            let mut owners = packages
                .members
                .iter()
                .filter_map(|id| self.objects.get(id))
                .collect::<Vec<_>>();
            owners.sort_by(|left, right| {
                left.name
                    .cmp(&right.name)
                    .then_with(|| left.id.cmp(&right.id))
            });
            for owner in owners {
                let _ = writeln!(text, "  {}", self.label(&owner.id));
                let public = self.public_namespaces(&owner.id);
                if public.len() > 1 {
                    let _ = writeln!(
                        text,
                        "    public namespaces: {}",
                        public
                            .iter()
                            .map(|namespace| namespace.name.as_str())
                            .collect::<Vec<_>>()
                            .join(" -> ")
                    );
                }
                if owner.package_exports.is_none() {
                    let _ = writeln!(text, "    PackageExports: unknown");
                }
                for namespace in &namespaces {
                    if namespace.owner_package.as_ref() == Some(&owner.id) {
                        namespace.render(&mut text, "    ");
                        rendered.insert(namespace.id.clone());
                    }
                }
            }
        }
        for namespace in namespaces {
            if !rendered.contains(&namespace.id) {
                namespace.render(&mut text, "");
            }
        }
        text.push_str("\nPackageExports (autoload public dependencies)\n");
        for export in &self.package_exports {
            let dependency = export
                .dependency
                .as_ref()
                .map(|dependency| self.label(dependency))
                .unwrap_or_else(|| format!("{} (unresolved)", export.name));
            let _ = writeln!(text, "{} -> {dependency}", self.label(&export.package));
        }
        text.push_str("\nparentOf (inheritance; used for subtype checks)\n");
        for object in self
            .objects
            .values()
            .filter(|object| object.type_id.is_some())
        {
            let _ = writeln!(
                text,
                "{} -> {}",
                self.label(&object.id),
                object
                    .parent_of
                    .as_ref()
                    .map(|parent| self.label(parent.object()))
                    .unwrap_or_else(|| "unknown".to_string())
            );
        }
        text.push_str("\nclassOf (runtime class; includes type and package objects)\n");
        for object in self.objects.values() {
            let _ = writeln!(
                text,
                "{} -> {}",
                self.label(&object.id),
                self.class_of(&object.id)
                    .map(|class| self.label(class.object()))
                    .unwrap_or_else(|| "unknown".to_string())
            );
        }
        text
    }

    /// Unambiguous label preserving canonical identity beside the display name.
    fn label(&self, id: &ObjectId) -> String {
        self.objects.get(id).map_or_else(
            || format!("{} (unresolved)", id.name()),
            |object| {
                let public_namespace = match &object.namespace {
                    NamespaceId::PackagePrivate(owner) => NamespaceId::Package(owner.clone()),
                    other => other.clone(),
                };
                let namespace = self
                    .namespaces
                    .iter()
                    .find(|namespace| namespace.id == public_namespace);
                let qualified = namespace
                    .and_then(|namespace| {
                        let direct = namespace.qualified_name(&object.name)?;
                        if namespace.qualified_bindings.get(&direct) == Some(id) {
                            return Some(direct);
                        }
                        namespace
                            .qualified_bindings
                            .iter()
                            .find(|(_, value)| *value == id)
                            .map(|(name, _)| name.clone())
                    })
                    .unwrap_or_else(|| object.name.clone());
                format!("{qualified} [{}]", id.name())
            },
        )
    }
}

impl TypeStore for ObjectHierarchy {
    fn parent_type_id(&self, type_id: &TypeId) -> Option<TypeId> {
        self.objects.get(type_id.object())?.parent_of.clone()
    }

    fn has_strict_subtype_id(&self, type_id: &TypeId) -> bool {
        !self.subtypes(type_id).is_empty()
    }
}

/// A retained workspace file's source facts and its surrounding package environment.
#[derive(Debug, Clone)]
pub struct HierarchySnapshot {
    /// Source objects extracted from the existing analyzed snapshot.
    pub source: ObjectHierarchy,
    /// Package visibility of this file, sharing the immutable catalog.
    pub registry: ObjectRegistry,
    /// Package lookup order, highest precedence first.
    pub packages: Vec<String>,
    /// Package session inherited by a file loaded inside a package declaration.
    pub package_context: Option<String>,
    /// Explicit imports, including source packages absent from the catalog.
    pub imports: Vec<String>,
}

impl HierarchySnapshot {
    /// Captures existing analysis; never reparses or executes the document.
    pub fn new(document: &DocumentSnapshot, uri: &Url) -> Self {
        Self {
            source: ObjectHierarchy::source_document(document, uri),
            package_context: None,
            imports: document
                .imported_packages()
                .iter()
                .map(|import| import.package.name().to_string())
                .collect(),
            registry: document.object_registry().clone(),
            packages: document
                .object_registry()
                .loaded_packages_at(pos_max!())
                .iter()
                .map(|name| name.name().to_string())
                .collect(),
        }
    }
}

/// Report returned by the workspace inspection command and printed in its log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceHierarchy {
    /// Combined object and namespace graph of the indexed workspace.
    pub hierarchy: ObjectHierarchy,
    /// Per-file lookup precedence; packages are not flattened into one global order.
    pub package_lookup: BTreeMap<Url, Vec<String>>,
    /// Explicit source imports; unknown packages remain visible here as names.
    pub imports: BTreeMap<Url, Vec<String>>,
    /// Readable rendering of the same graph returned structurally above.
    pub text: String,
}

impl WorkspaceHierarchy {
    /// Editor command returning this report and printing its text in the output log.
    pub const COMMAND: &str = "m2-ls.printHierarchies";

    /// Merges retained files and includes the environment used by each file.
    pub fn new(snapshots: BTreeMap<Url, HierarchySnapshot>, baseline: &ObjectRegistry) -> Self {
        let mut hierarchy = ObjectHierarchy::default();
        let mut package_lookup = BTreeMap::new();
        let mut imports = BTreeMap::new();
        let mut contexts = Vec::new();
        if snapshots.is_empty() {
            hierarchy.include_environment(baseline, pos_max!());
            hierarchy.include_user_package(baseline);
        }
        for (uri, snapshot) in snapshots {
            hierarchy.include_environment(&snapshot.registry, pos_max!());
            hierarchy.merge(snapshot.source);
            if let Some(package) = snapshot.package_context {
                contexts.push((uri.clone(), package));
            }
            imports.insert(uri.clone(), snapshot.imports);
            package_lookup.insert(uri, snapshot.packages);
        }
        for (uri, package) in contexts {
            hierarchy.attach_to_package(&uri, &package);
        }
        hierarchy.resolve_exports();
        hierarchy.resolve_packages();
        hierarchy.resolve_package_exports();
        let mut text = hierarchy.render();
        text.push_str("\nPackage lookup (highest precedence first, per document)\n");
        for (uri, packages) in &package_lookup {
            let _ = writeln!(text, "{uri}: {}", packages.join(" -> "));
        }
        text.push_str("\nExplicit imports (source order)\n");
        for (uri, names) in &imports {
            let _ = writeln!(text, "{uri}: {}", names.join(" -> "));
        }
        Self {
            hierarchy,
            package_lookup,
            imports,
            text,
        }
    }
}
