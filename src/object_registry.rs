//! Canonical object identities, typed object handles, and registry lookup.

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::fmt::Formatter;
use std::fmt::{Debug, Display, Result};
use std::sync::Arc;

use crate::builtin_index::{BuiltinIndex, OptionFacts, Record};
#[cfg(test)]
use crate::package_index::collect_imported_packages;
use crate::package_index::PackageImport;
use tower_lsp::lsp_types::Position;

/// Nominal spelling used to resolve a Macaulay2 object in an environment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectName(pub String);

impl ObjectName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn name(&self) -> &str {
        &self.0
    }
}

impl Display for ObjectName {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> Result {
        formatter.write_str(self.name())
    }
}

impl Borrow<str> for ObjectName {
    fn borrow(&self) -> &str {
        self.name()
    }
}

impl AsRef<str> for ObjectName {
    fn as_ref(&self) -> &str {
        self.name()
    }
}

/// Syntactic form in which an operator is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperatorForm {
    Binary,
    Prefix,
    Postfix,
    Assignment,
}

impl Display for OperatorForm {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> Result {
        formatter.write_str(match self {
            Self::Binary => "binary",
            Self::Prefix => "prefix",
            Self::Postfix => "postfix",
            Self::Assignment => "assignment",
        })
    }
}

/// Name and membership indexes for one package's objects.
#[derive(Debug, Clone, Default)]
pub struct PackageObjects {
    pub objects: Vec<ObjectId>,
    pub objects_by_name: HashMap<ObjectName, ObjectId>,
}

/// Immutable indexed object population from which package registries are loaded.
#[derive(Debug)]
struct ObjectCatalog {
    index: BuiltinIndex,
    option_facts: OptionFacts,
    packages: HashMap<ObjectId, PackageObjects>,
    packages_by_name: HashMap<ObjectName, ObjectId>,
    default_loaded: Vec<String>,
}

/// Ordered registry of the packages loaded for one Macaulay2 environment.
#[derive(Debug, Clone)]
pub struct ObjectRegistry {
    catalog: Arc<ObjectCatalog>,
    packages: Vec<PackageRegistration>,
}

/// One source-ordered package inclusion in an object registry.
#[derive(Debug, Clone)]
struct PackageRegistration {
    package: ObjectId,
    effective_from: Option<Position>,
}

/// Range-specific view of an object registry.
#[derive(Debug, Clone, Copy)]
pub struct ObjectRegistryView<'registry> {
    registry: &'registry ObjectRegistry,
    position: Position,
}

impl Default for ObjectRegistry {
    fn default() -> Self {
        Self::from_index(BuiltinIndex::default())
    }
}

impl ObjectRegistry {
    pub fn load(corpus: &str) -> Self {
        Self::from_index(BuiltinIndex::load(corpus))
    }

    pub fn len(&self) -> usize {
        let mut seen = HashSet::new();
        self.packages
            .iter()
            .filter(|registration| seen.insert(&registration.package))
            .filter_map(|registration| self.catalog.packages.get(&registration.package))
            .map(|package| package.objects.len())
            .sum()
    }

    pub fn from_index(index: BuiltinIndex) -> Self {
        let option_facts = OptionFacts::from_records(index.records());
        let default_loaded = index.default_loaded_packages().to_vec();
        let mut package_objects: HashMap<ObjectId, PackageObjects> = HashMap::new();
        for record in index.records() {
            let package = package_objects.entry(record.package.clone()).or_default();
            package.objects.push(record.id.clone());
            for name in record.lookup_names() {
                package
                    .objects_by_name
                    .entry(name)
                    .or_insert_with(|| record.id.clone());
            }
        }
        let packages_by_name = package_objects
            .keys()
            .map(|package| {
                let package_record = index
                    .object(package)
                    .expect("every package identity must name a registered object");
                (package_record.name.clone(), package.clone())
            })
            .collect();
        let catalog = Arc::new(ObjectCatalog {
            index,
            option_facts,
            packages: package_objects,
            packages_by_name,
            default_loaded,
        });
        let mut registry = Self {
            catalog,
            packages: Vec::new(),
        };
        let mut baseline = registry.catalog.default_loaded.clone();
        if baseline.is_empty() {
            baseline.push("Core".to_string());
        }
        for package in baseline {
            registry.load_package(&ObjectName::new(package));
        }
        registry
    }

    pub fn load_package(&mut self, name: &ObjectName) -> bool {
        self.register_package(name, None)
    }

    pub fn load_package_at(&mut self, name: &ObjectName, position: Position) -> bool {
        self.register_package(name, Some(position))
    }

    pub fn with_imports(&self, imports: &[PackageImport]) -> Self {
        let mut registry = self.clone();
        for import in imports {
            registry.load_package_at(&import.package, import.effective_from);
        }
        registry
    }

    #[cfg(test)]
    pub fn with_source_imports(&self, text: &str) -> Self {
        self.with_imports(&collect_imported_packages(text))
    }

    fn register_package(&mut self, name: &ObjectName, effective_from: Option<Position>) -> bool {
        let Some(package) = self.catalog.packages_by_name.get(name) else {
            return false;
        };
        if effective_from.is_none()
            && self.packages.iter().any(|registration| {
                registration.package == *package && registration.effective_from.is_none()
            })
        {
            return true;
        }
        self.packages.push(PackageRegistration {
            package: package.clone(),
            effective_from,
        });
        true
    }

    #[cfg(test)]
    pub fn package_id(&self, name: &ObjectName) -> Option<&ObjectId> {
        self.package_id_at(name, pos_max!())
    }

    #[cfg(test)]
    pub fn package_id_at(&self, name: &ObjectName, position: Position) -> Option<&ObjectId> {
        let package = self.catalog.packages_by_name.get(name)?;
        self.packages
            .iter()
            .rev()
            .find(|registration| {
                &registration.package == package && registration.is_effective_at(position)
            })
            .map(|registration| &registration.package)
    }

    pub fn package_name(&self, package: &ObjectId) -> Option<&str> {
        self.object(package).map(|record| record.name.name())
    }

    pub fn package_names_with_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        let mut names = self
            .catalog
            .packages_by_name
            .keys()
            .map(ObjectName::name)
            .filter(|name| name.starts_with(prefix))
            .map(str::to_string)
            .collect::<Vec<_>>();
        names.sort();
        names.truncate(limit);
        names
    }

    /// Packages preloaded by a normal session, as recorded by the library index.
    /// Explicit package loads in a document do not change this list.
    pub fn default_loaded(&self) -> &[String] {
        &self.catalog.default_loaded
    }

    pub fn records_by_precedence(&self) -> impl Iterator<Item = &Record> {
        self.records_by_precedence_at(pos_max!())
    }

    pub fn records_by_precedence_at(&self, position: Position) -> impl Iterator<Item = &Record> {
        let mut seen = HashSet::new();
        self.packages
            .iter()
            .rev()
            .filter(move |registration| registration.is_effective_at(position))
            .filter(move |registration| seen.insert(&registration.package))
            .filter_map(|registration| self.catalog.packages.get(&registration.package))
            .flat_map(|package| package.objects.iter())
            .filter_map(|object| self.catalog.index.object(object))
    }

    /// The packages loaded at `position`, most recently loaded first.
    pub fn loaded_packages_at(&self, position: Position) -> Vec<ObjectName> {
        let mut seen = HashSet::new();
        self.packages
            .iter()
            .rev()
            .filter(|registration| registration.is_effective_at(position))
            .filter(|registration| seen.insert(&registration.package))
            .filter_map(|registration| self.package_name(&registration.package))
            .map(ObjectName::new)
            .collect()
    }

    pub fn option_facts(&self) -> &OptionFacts {
        &self.catalog.option_facts
    }

    pub fn package_objects(&self, package: &ObjectId) -> Option<&PackageObjects> {
        self.catalog.packages.get(package)
    }

    pub fn catalog_package_id(&self, name: &ObjectName) -> Option<&ObjectId> {
        self.catalog.packages_by_name.get(name)
    }

    pub fn catalog_records(&self) -> &[Record] {
        self.catalog.index.records()
    }

    pub fn at(&self, position: Position) -> ObjectRegistryView<'_> {
        ObjectRegistryView {
            registry: self,
            position,
        }
    }
}

impl PackageRegistration {
    fn is_effective_at(&self, position: Position) -> bool {
        self.effective_from
            .is_none_or(|effective_from| effective_from <= position)
    }
}

impl ObjectRegistryView<'_> {
    pub fn at(&self, position: Position) -> Self {
        self.registry.at(position.min(self.position))
    }

    pub fn records_by_precedence(&self) -> impl Iterator<Item = &Record> {
        self.registry.records_by_precedence_at(self.position)
    }

    pub fn package_name(&self, package: &ObjectId) -> Option<&str> {
        self.registry.package_name(package)
    }

    pub fn loaded_packages(&self) -> Vec<ObjectName> {
        self.registry.loaded_packages_at(self.position)
    }

    pub fn package_names_with_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        self.registry.package_names_with_prefix(prefix, limit)
    }

    pub fn option_facts(&self) -> &OptionFacts {
        self.registry.option_facts()
    }

    pub fn shadows_source(&self, name: &ObjectName, source_position: Position) -> bool {
        self.registry
            .registration_for_name_at(name, self.position)
            .and_then(|(_, effective_from)| effective_from)
            .is_some_and(|effective_from| effective_from > source_position)
    }
}

/// Canonical symbol identity of one named Macaulay2 object.
///
/// Package records retain their qualified corpus symbol (for example
/// `$Core$Thing`). That qualification is internal to the linked corpus; source
/// lookup uses [`ObjectName`] spellings and aliases.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(ObjectName);

impl ObjectId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(ObjectName(name.into()))
    }

    pub fn name(&self) -> &str {
        self.0.name()
    }
}

impl Borrow<ObjectName> for ObjectId {
    fn borrow(&self) -> &ObjectName {
        &self.0
    }
}

/// Identity of an object known to be a Macaulay2 type.
///
/// Indexed identities require registry validation; source identities require a
/// validated parent and the reserved source namespace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeId(ObjectId);

impl TypeId {
    fn from_validated_object(object: ObjectId) -> Self {
        Self(object)
    }

    pub fn from_source_type(object: ObjectId, _parent: &TypeId) -> Self {
        assert!(
            object.name().starts_with("$Source$"),
            "source type identities must use the reserved source namespace"
        );
        Self(object)
    }

    pub fn object(&self) -> &ObjectId {
        &self.0
    }
}

/// Direct parent edge of one registered Macaulay2 type.
#[derive(Debug, Clone)]
pub struct TypeData {
    pub parent: Option<TypeId>,
}

/// Direct parent access required to navigate the type partial order.
pub trait TypeStore {
    fn parent_type_id(&self, type_id: &TypeId) -> Option<TypeId>;

    fn has_strict_subtype_id(&self, type_id: &TypeId) -> bool;

    fn is_subtype_id(&self, child: &TypeId, parent: &TypeId) -> bool {
        let mut current = child.clone();
        let mut visited = HashSet::new();
        loop {
            if current == *parent {
                return true;
            }
            if !visited.insert(current.clone()) {
                return false;
            }
            let Some(next) = self.parent_type_id(&current) else {
                return false;
            };
            if next == current {
                return false;
            }
            current = next;
        }
    }
}

/// Identity and record lookup shared by every semantic object source.
pub trait ObjectKnowledge: TypeStore {
    fn object(&self, object_id: &ObjectId) -> Option<&Record>;

    fn resolve_object(&self, name: &ObjectName) -> Option<ObjectId>;

    fn get_record(&self, name: &ObjectName) -> Option<&Record> {
        let object = self.resolve_object(name)?;
        self.object(&object)
    }

    fn resolve_type_id(&self, name: &ObjectName) -> Option<TypeId> {
        let object = self.resolve_object(name)?;
        self.type_id(&object)
    }

    fn type_id(&self, object: &ObjectId) -> Option<TypeId> {
        self.object(object)?.type_info()?;
        Some(TypeId::from_validated_object(object.clone()))
    }

    fn type_name(&self, type_id: &TypeId) -> Option<&ObjectName> {
        let record = self.object(type_id.object())?;
        record.type_info()?;
        Some(&record.name)
    }
}

impl ObjectRegistry {
    pub fn get_record(&self, name: &ObjectName) -> Option<&Record> {
        let object = self.object_id(name)?;
        self.object(&object)
    }

    pub fn object(&self, object_id: &ObjectId) -> Option<&Record> {
        self.catalog.index.object(object_id)
    }

    pub fn object_id(&self, name: &ObjectName) -> Option<ObjectId> {
        self.object_id_at(name, pos_max!())
    }

    pub fn object_id_at(&self, name: &ObjectName, position: Position) -> Option<ObjectId> {
        self.registration_for_name_at(name, position)
            .map(|(object, _)| object)
    }

    fn registration_for_name_at(
        &self,
        name: &ObjectName,
        position: Position,
    ) -> Option<(ObjectId, Option<Position>)> {
        self.packages.iter().rev().find_map(|registration| {
            if !registration.is_effective_at(position) {
                return None;
            }
            let object = self
                .catalog
                .packages
                .get(&registration.package)?
                .objects_by_name
                .get(name)
                .cloned()?;
            Some((object, registration.effective_from))
        })
    }
}

impl BuiltinIndex {
    pub fn type_id(&self, object: &ObjectId) -> Option<TypeId> {
        self.object(object)?.type_info()?;
        Some(TypeId::from_validated_object(object.clone()))
    }
}

impl ObjectKnowledge for ObjectRegistry {
    fn object(&self, object_id: &ObjectId) -> Option<&Record> {
        ObjectRegistry::object(self, object_id)
    }

    fn resolve_object(&self, name: &ObjectName) -> Option<ObjectId> {
        ObjectRegistry::object_id(self, name)
    }
}

impl ObjectKnowledge for ObjectRegistryView<'_> {
    fn object(&self, object_id: &ObjectId) -> Option<&Record> {
        self.registry.object(object_id)
    }

    fn resolve_object(&self, name: &ObjectName) -> Option<ObjectId> {
        self.registry.object_id_at(name, self.position)
    }
}

impl TypeStore for ObjectRegistry {
    fn parent_type_id(&self, type_id: &TypeId) -> Option<TypeId> {
        self.object(type_id.object())?
            .type_info()
            .and_then(|data| data.parent.clone())
    }

    fn has_strict_subtype_id(&self, type_id: &TypeId) -> bool {
        self.records_by_precedence().any(|record| {
            record.id != *type_id.object()
                && record.type_info().and_then(|data| data.parent.as_ref()) == Some(type_id)
        })
    }
}

impl TypeStore for ObjectRegistryView<'_> {
    fn parent_type_id(&self, type_id: &TypeId) -> Option<TypeId> {
        self.object(type_id.object())?
            .type_info()
            .and_then(|data| data.parent.clone())
    }

    fn has_strict_subtype_id(&self, type_id: &TypeId) -> bool {
        self.registry
            .records_by_precedence_at(self.position)
            .any(|record| {
                record.id != *type_id.object()
                    && record.type_info().and_then(|data| data.parent.as_ref()) == Some(type_id)
            })
    }
}

impl<T: TypeStore + ?Sized> TypeStore for &T {
    fn parent_type_id(&self, type_id: &TypeId) -> Option<TypeId> {
        T::parent_type_id(self, type_id)
    }

    fn has_strict_subtype_id(&self, type_id: &TypeId) -> bool {
        T::has_strict_subtype_id(self, type_id)
    }
}

impl<T: ObjectKnowledge + ?Sized> ObjectKnowledge for &T {
    fn object(&self, object_id: &ObjectId) -> Option<&Record> {
        T::object(self, object_id)
    }

    fn resolve_object(&self, name: &ObjectName) -> Option<ObjectId> {
        T::resolve_object(self, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record_lsp::LspKnowledge;

    fn corpus() -> &'static str {
        include_str!("./data/m2-index.jsonl")
    }

    #[test]
    fn from_corpus_builds_a_core_partition() {
        let registry = ObjectRegistry::load(corpus());
        assert!(registry.package_id(&ObjectName::new("Core")).is_some());
        assert!(registry.get_record(&ObjectName::new("ZZ")).is_some());
    }

    #[test]
    fn default_loaded_uses_meta_record_when_present() {
        let synthetic = concat!(
            r#"{"kind":"meta","default_loaded":["Core","Classic"]}"#,
            "\n",
            r#"{"kind":"type","name":"ZZ","package":"$Core$Core"}"#,
        );
        let index = ObjectRegistry::load(synthetic);
        assert_eq!(index.default_loaded(), &["Core", "Classic"]);
    }

    #[test]
    fn curated_extras_are_not_loaded_into_the_baseline_registry() {
        let registry = ObjectRegistry::load(corpus());
        assert!(
            !registry.default_loaded().iter().any(|p| p == "JSON"),
            "JSON is a non-default package and must stay out of the baseline"
        );
        assert!(registry.package_id(&ObjectName::new("JSON")).is_none());
        assert!(registry.get_record(&ObjectName::new("toJSON")).is_none());
        assert!(registry.get_record(&ObjectName::new("fromJSON")).is_none());
    }

    #[test]
    fn loaded_packages_picks_up_an_imported_extra() {
        let registry = ObjectRegistry::load(corpus());

        let imported = registry.with_source_imports("needsPackage \"JSON\"");
        assert!(imported.get_record(&ObjectName::new("toJSON")).is_some());

        let plain = registry.with_source_imports("1 + 1");
        assert!(plain.get_record(&ObjectName::new("toJSON")).is_none());
    }

    #[test]
    fn removing_an_import_unloads_the_package() {
        let registry = ObjectRegistry::load(corpus());

        let with_import = registry.with_source_imports("needsPackage \"JSON\"\n1 + 1");
        assert!(with_import.get_record(&ObjectName::new("toJSON")).is_some());

        let after_removal = registry.with_source_imports("1 + 1");
        assert!(
            after_removal
                .get_record(&ObjectName::new("toJSON"))
                .is_none(),
            "deleting the import line drops the package from the loaded set"
        );
    }

    #[test]
    fn scoped_resolves_only_loaded_partitions() {
        let registry = ObjectRegistry::load(corpus());

        // Baseline only: Core resolves, JSON does not.
        let scoped = registry.with_source_imports("1 + 1");
        assert!(scoped.get_record(&ObjectName::new("ZZ")).is_some());
        assert!(scoped.get_record(&ObjectName::new("toJSON")).is_none());
        let zz_id = scoped
            .resolve_object(&ObjectName::new("ZZ"))
            .expect("loaded objects resolve to identities");
        assert_eq!(
            scoped.object(&zz_id).map(|record| &record.name),
            Some(&ObjectName::new("ZZ"))
        );

        // Import JSON: now toJSON resolves, tagged with its package.
        let scoped = registry.with_source_imports("needsPackage \"JSON\"");
        assert!(scoped.package_id(&ObjectName::new("JSON")).is_some());
        let (pkg, record) = scoped
            .get_record_with_package(&ObjectName::new("toJSON"))
            .expect("toJSON resolves once JSON is loaded");
        assert_eq!(pkg, "JSON");
        assert_eq!(record.name.0, "toJSON");
    }

    #[test]
    fn scoped_skips_imports_absent_from_corpus() {
        let registry = ObjectRegistry::load(corpus());
        let scoped = registry.with_source_imports("needsPackage \"NoSuchPkg\"");
        assert!(scoped.get_record(&ObjectName::new("ZZ")).is_some());
    }

    #[test]
    fn partial_corpus_registers_qualified_placeholder_packages() {
        let corpus = concat!(
            r#"{"kind":"meta","default_loaded":["Core"]}"#,
            "\n",
            r#"{"kind":"type","name":"T","package":"$Core$Core"}"#,
            "\n",
            r#"{"kind":"function","name":"f","package":"$Core$Core","methods":[{"domain":["$Foo$T"]}]}"#,
        );
        let registry = ObjectRegistry::load(corpus);
        let imported = registry.with_source_imports("needsPackage \"Foo\"");

        assert!(imported.package_id(&ObjectName::new("Foo")).is_some());
        let (package, _) = imported
            .get_record_with_package(&ObjectName::new("T"))
            .expect("qualified placeholder resolves from its imported package");
        assert_eq!(package, "Foo");
    }

    #[test]
    fn unresolved_bare_placeholders_are_not_exported_from_core() {
        let corpus = concat!(
            r#"{"kind":"meta","default_loaded":["Core"]}"#,
            "\n",
            r#"{"kind":"function","name":"f","package":"$Core$Core","methods":[{"domain":["MissingType"]}]}"#,
        );
        let registry = ObjectRegistry::load(corpus);

        assert!(registry
            .get_record(&ObjectName::new("MissingType"))
            .is_none());
    }
}
