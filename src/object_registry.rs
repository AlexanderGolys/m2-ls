//! Canonical object identities, typed object handles, and registry lookup.

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::fmt::Formatter;
use std::fmt::{Debug, Display, Result};
use std::sync::Arc;

use serde::Deserialize;

use crate::builtin_index::{BuiltinIndex, OptionFacts, Record};
use crate::init_file::InitFileStart;
#[cfg(test)]
use crate::package_index::collect_imported_packages;
use crate::package_index::PackageImport;
use tower_lsp::lsp_types::Position;

/// Nominal spelling used to resolve a Macaulay2 object in an environment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(transparent)]
pub struct ObjectName(pub String);

impl ObjectName {
    /// The name spelled `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The name as spelled.
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
    /// Between two operands: `x + y`.
    Binary,
    /// Before its operand: `-x`.
    Prefix,
    /// After its operand: `n!`.
    Postfix,
    /// As an assignment: `x += y`.
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
    /// The package's objects, in catalog order.
    pub objects: Vec<ObjectId>,
    /// Each object by every name it is looked up by, its aliases included.
    pub objects_by_name: HashMap<ObjectName, ObjectId>,
}

/// Immutable indexed object population from which package registries are loaded.
#[derive(Debug)]
struct ObjectCatalog {
    /// Every record of the catalog.
    index: BuiltinIndex,
    /// The options functions accept, gathered from the records.
    option_facts: OptionFacts,
    /// The objects of each package.
    packages: HashMap<ObjectId, PackageObjects>,
    /// Each package by its name.
    packages_by_name: HashMap<ObjectName, ObjectId>,
    /// The packages a normal session preloads.
    default_loaded: Vec<String>,
}

/// Ordered registry of the packages loaded for one Macaulay2 environment.
#[derive(Debug, Clone)]
pub struct ObjectRegistry {
    /// The catalog the packages are loaded from, shared by every registry.
    catalog: Arc<ObjectCatalog>,
    /// The packages loaded, in loading order: a later one hides the names of an
    /// earlier one.
    packages: Vec<PackageRegistration>,
    /// The init file the session starts with, whose packages are loaded.
    init_file: InitFileStart,
}

/// One source-ordered package inclusion in an object registry.
#[derive(Debug, Clone)]
struct PackageRegistration {
    /// The package loaded.
    package: ObjectId,
    /// Where its names become visible; `None` from the start of the source.
    effective_from: Option<Position>,
    /// Whether the session's init file loaded it.
    loaded_by_init_file: bool,
}

/// Range-specific view of an object registry.
#[derive(Debug, Clone, Copy)]
pub struct ObjectRegistryView<'registry> {
    /// The registry viewed.
    registry: &'registry ObjectRegistry,
    /// The source position whose loaded packages the view sees.
    position: Position,
}

impl Default for ObjectRegistry {
    fn default() -> Self {
        Self::from_index(BuiltinIndex::default())
    }
}

impl ObjectRegistry {
    /// The registry of the catalog `corpus`, one JSON record per line, with the
    /// default packages loaded.
    pub fn load(corpus: &str) -> Self {
        Self::from_index(BuiltinIndex::load(corpus))
    }

    /// How many objects the loaded packages hold, each package counted once.
    pub fn len(&self) -> usize {
        let mut seen = HashSet::new();
        self.packages
            .iter()
            .filter(|registration| seen.insert(&registration.package))
            .filter_map(|registration| self.catalog.packages.get(&registration.package))
            .map(|package| package.objects.len())
            .sum()
    }

    /// The registry of `index` with the packages a normal session preloads
    /// loaded, or Core alone when the index records none.
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
            init_file: InitFileStart::None,
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

    /// Load the package `name` from the start of the source; returns whether
    /// the catalog knows it.
    pub fn load_package(&mut self, name: &ObjectName) -> bool {
        self.register_package(name, None, false)
    }

    /// Load the package `name` from `position` on; returns whether the catalog
    /// knows it.
    pub fn load_package_at(&mut self, name: &ObjectName, position: Position) -> bool {
        self.register_package(name, Some(position), false)
    }

    /// The registry with each of `imports` loaded from where it takes effect.
    pub fn with_imports(&self, imports: &[PackageImport]) -> Self {
        let mut registry = self.clone();
        for import in imports {
            registry.load_package_at(&import.package, import.effective_from);
        }
        registry
    }

    /// An implementation source starts before the session's automatic imports.
    /// Catalog identities remain available for explicit package loads.
    pub fn without_prelude(&self) -> Self {
        Self {
            catalog: Arc::clone(&self.catalog),
            packages: Vec::new(),
            init_file: InitFileStart::None,
        }
    }

    /// The session started with `init_file` in place of the init file this
    /// one starts with: the packages it loads are loaded from the start.
    pub fn with_init_file(&self, init_file: InitFileStart) -> Self {
        let mut registry = self.without_init_file();
        for package in init_file.file().map_or(&[][..], |file| file.packages()) {
            registry.register_package(package, None, true);
        }
        registry.init_file = init_file;
        registry
    }

    /// The session started without an init file.
    pub fn without_init_file(&self) -> Self {
        Self {
            catalog: Arc::clone(&self.catalog),
            packages: self
                .packages
                .iter()
                .filter(|registration| !registration.loaded_by_init_file)
                .cloned()
                .collect(),
            init_file: InitFileStart::None,
        }
    }

    /// The init file the session starts with.
    pub fn init_file(&self) -> &InitFileStart {
        &self.init_file
    }

    #[cfg(test)]
    pub fn with_source_imports(&self, text: &str) -> Self {
        self.with_imports(&collect_imported_packages(text))
    }

    /// Load the package `name` from `effective_from`, recording whether the
    /// init file loads it; a package already loaded from the start is not
    /// loaded again. Returns whether the catalog knows it.
    fn register_package(
        &mut self,
        name: &ObjectName,
        effective_from: Option<Position>,
        loaded_by_init_file: bool,
    ) -> bool {
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
            loaded_by_init_file,
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

    /// The name of the package `package`.
    pub fn package_name(&self, package: &ObjectId) -> Option<&str> {
        self.object(package).map(|record| record.name.name())
    }

    /// The names of the catalog's packages starting with `prefix`, sorted, at
    /// most `limit` of them.
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

    /// The records of the loaded packages, the last loaded first, each package
    /// once.
    pub fn records_by_precedence(&self) -> impl Iterator<Item = &Record> {
        self.records_by_precedence_at(pos_max!())
    }

    /// The records of the packages loaded at `position`, the last loaded first,
    /// each package once.
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

    /// The options functions accept.
    pub fn option_facts(&self) -> &OptionFacts {
        &self.catalog.option_facts
    }

    /// The objects of the package `package`, loaded or not.
    pub fn package_objects(&self, package: &ObjectId) -> Option<&PackageObjects> {
        self.catalog.packages.get(package)
    }

    /// The package named `name` in the catalog, loaded or not.
    pub fn catalog_package_id(&self, name: &ObjectName) -> Option<&ObjectId> {
        self.catalog.packages_by_name.get(name)
    }

    /// Every record of the catalog, loaded or not.
    pub fn catalog_records(&self) -> &[Record] {
        self.catalog.index.records()
    }

    /// The registry as the source sees it at `position`: only the packages
    /// loaded by then.
    pub fn at(&self, position: Position) -> ObjectRegistryView<'_> {
        ObjectRegistryView {
            registry: self,
            position,
        }
    }
}

impl PackageRegistration {
    /// Whether the package's names are visible at `position`.
    fn is_effective_at(&self, position: Position) -> bool {
        self.effective_from
            .is_none_or(|effective_from| effective_from <= position)
    }
}

impl ObjectRegistryView<'_> {
    /// The view at `position`, or at the view's own position if that is
    /// earlier.
    pub fn at(&self, position: Position) -> Self {
        self.registry.at(position.min(self.position))
    }

    /// The records of the packages loaded at the view's position, the last
    /// loaded first.
    pub fn records_by_precedence(&self) -> impl Iterator<Item = &Record> {
        self.registry.records_by_precedence_at(self.position)
    }

    /// The name of the package `package`.
    pub fn package_name(&self, package: &ObjectId) -> Option<&str> {
        self.registry.package_name(package)
    }

    /// The packages loaded at the view's position, the last loaded first.
    pub fn loaded_packages(&self) -> Vec<ObjectName> {
        self.registry.loaded_packages_at(self.position)
    }

    /// The names of the catalog's packages starting with `prefix`, sorted, at
    /// most `limit` of them.
    pub fn package_names_with_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        self.registry.package_names_with_prefix(prefix, limit)
    }

    /// The options functions accept.
    pub fn option_facts(&self) -> &OptionFacts {
        self.registry.option_facts()
    }

    /// Whether a package imported after `source_position` binds `name` at the
    /// view's position, hiding the source definition made there.
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
    /// The identity spelled `name`, qualified as the catalog qualifies it.
    pub fn new(name: impl Into<String>) -> Self {
        Self(ObjectName(name.into()))
    }

    /// The identity as spelled.
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
    /// The type identity of `object`, which the caller checked is a type.
    fn from_validated_object(object: ObjectId) -> Self {
        Self(object)
    }

    /// The type identity of a type the source defines, under the reserved
    /// `$Source$` namespace, whose parent is the type `_parent`.
    pub fn from_source_type(object: ObjectId, _parent: &TypeId) -> Self {
        assert!(
            object.name().starts_with("$Source$"),
            "source type identities must use the reserved source namespace"
        );
        Self(object)
    }

    /// The object the type is.
    pub fn object(&self) -> &ObjectId {
        &self.0
    }
}

/// Direct parent edge of one registered Macaulay2 type.
#[derive(Debug, Clone)]
pub struct TypeData {
    /// The type's direct parent; `None` for `Thing`.
    pub parent: Option<TypeId>,
}

/// Direct parent access required to navigate the type partial order.
pub trait TypeStore {
    /// The direct parent of `type_id`.
    fn parent_type_id(&self, type_id: &TypeId) -> Option<TypeId>;

    /// Whether some type has `type_id` as a proper ancestor.
    fn has_strict_subtype_id(&self, type_id: &TypeId) -> bool;

    /// Whether `child` is `parent` or descends from it, following parent edges
    /// and stopping at a cycle.
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
    /// The record of `object_id`.
    fn object(&self, object_id: &ObjectId) -> Option<&Record>;

    /// The object `name` resolves to.
    fn resolve_object(&self, name: &ObjectName) -> Option<ObjectId>;

    /// The record of the object `name` resolves to.
    fn get_record(&self, name: &ObjectName) -> Option<&Record> {
        let object = self.resolve_object(name)?;
        self.object(&object)
    }

    /// The type `name` resolves to, if it names a type.
    fn resolve_type_id(&self, name: &ObjectName) -> Option<TypeId> {
        let object = self.resolve_object(name)?;
        self.type_id(&object)
    }

    /// The type identity of `object`, if it is a type.
    fn type_id(&self, object: &ObjectId) -> Option<TypeId> {
        self.object(object)?.type_info()?;
        Some(TypeId::from_validated_object(object.clone()))
    }

    /// The name of the type `type_id`.
    fn type_name(&self, type_id: &TypeId) -> Option<&ObjectName> {
        let record = self.object(type_id.object())?;
        record.type_info()?;
        Some(&record.name)
    }
}

impl ObjectRegistry {
    /// The record of the object `name` resolves to in the loaded packages.
    pub fn get_record(&self, name: &ObjectName) -> Option<&Record> {
        let object = self.object_id(name)?;
        self.object(&object)
    }

    /// The record of `object_id`, loaded or not.
    pub fn object(&self, object_id: &ObjectId) -> Option<&Record> {
        self.catalog.index.object(object_id)
    }

    /// The object `name` resolves to once every package is loaded.
    pub fn object_id(&self, name: &ObjectName) -> Option<ObjectId> {
        self.object_id_at(name, pos_max!())
    }

    /// The object `name` resolves to at `position`, in the last package loaded
    /// by then that binds it.
    pub fn object_id_at(&self, name: &ObjectName, position: Position) -> Option<ObjectId> {
        self.registration_for_name_at(name, position)
            .map(|(object, _)| object)
    }

    /// The object `name` resolves to at `position`, with where the package
    /// binding it takes effect.
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
    /// The type identity of `object`, if it is a type.
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
        crate::catalog::test_corpus()
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
