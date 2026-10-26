use std::cmp::Ordering;
use std::collections::HashSet;
use std::hash::Hash;
use std::ops::{BitAnd, BitOr};
use std::sync::Arc;

use crate::object_registry::{ObjectName, TypeId, TypeStore};

use super::{SubtypeEvidence, TypeRole};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TypeKey(Arc<str>);

impl TypeKey {
    fn from_id(type_id: &TypeId) -> Self {
        Self(Arc::from(type_id.object().name()))
    }

    fn from_name(name: &ObjectName) -> Self {
        Self(Arc::from(name.name()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Type {
    name: ObjectName,
    ancestry: Arc<[TypeKey]>,
}

impl Type {
    pub fn direct(name: ObjectName) -> Self {
        let thing = TypeKey::from_name(&ObjectName::new("Thing"));
        let type_key = TypeKey::from_name(&name);
        let ancestry = if thing == type_key {
            vec![thing]
        } else {
            vec![thing, type_key]
        };
        Self {
            name,
            ancestry: ancestry.into(),
        }
    }

    pub fn from_id(
        name: ObjectName,
        type_id: TypeId,
        types: &(impl TypeStore + ?Sized),
    ) -> Option<Self> {
        let mut ancestry = Vec::new();
        let mut current = type_id;
        let mut visited = HashSet::new();
        loop {
            if !visited.insert(current.clone()) {
                return None;
            }
            ancestry.push(TypeKey::from_id(&current));
            let parent = types.parent_type_id(&current)?;
            if parent == current {
                break;
            }
            current = parent;
        }
        ancestry.reverse();
        Some(Self {
            name,
            ancestry: ancestry.into(),
        })
    }

    pub fn name(&self) -> &ObjectName {
        &self.name
    }
}

impl PartialOrd for Type {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self == other {
            return Some(Ordering::Equal);
        }
        if other.ancestry.starts_with(&self.ancestry) {
            return Some(Ordering::Less);
        }
        if self.ancestry.starts_with(&other.ancestry) {
            return Some(Ordering::Greater);
        }
        None
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeRange {
    exact: HashSet<Type>,
    generators: HashSet<Type>,
}

impl TypeRange {
    pub fn diverges() -> Self {
        Self::default()
    }

    pub fn exact(name: &str) -> Self {
        Self::exact_type(Type::direct(ObjectName::new(name)))
    }

    pub fn exact_from_id(name: ObjectName) -> Self {
        Self::exact_type(Type::direct(name))
    }

    pub fn exact_type(point: Type) -> Self {
        Self {
            exact: HashSet::from([point]),
            generators: HashSet::new(),
        }
    }

    pub fn upward(name: &str) -> Self {
        Self::upward_type(Type::direct(ObjectName::new(name)))
    }

    pub fn upward_from_id(name: ObjectName) -> Self {
        Self::upward_type(Type::direct(name))
    }

    pub fn upward_type(generator: Type) -> Self {
        Self {
            exact: HashSet::new(),
            generators: HashSet::from([generator]),
        }
    }

    pub fn unknown() -> Self {
        Self::upward("Thing")
    }

    /// Whether the range says nothing about the value: it reaches up to
    /// `Thing`, which every value is.
    pub fn is_unknown(&self) -> bool {
        let top = TypeRole::Thing.object_name();
        self.generators
            .iter()
            .any(|generator| *generator.name() == top)
    }

    pub fn single(&self) -> Option<&ObjectName> {
        match (self.exact.len(), self.generators.len()) {
            (1, 0) => self.exact.iter().next().map(Type::name),
            (0, 1) => self.generators.iter().next().map(Type::name),
            _ => None,
        }
    }

    pub fn exact_points(&self) -> impl Iterator<Item = &ObjectName> {
        self.exact.iter().map(Type::name)
    }

    pub fn upward_generators(&self) -> impl Iterator<Item = &ObjectName> {
        self.generators.iter().map(Type::name)
    }

    pub fn resolved_by(mut self, mut resolve: impl FnMut(&ObjectName) -> Option<Type>) -> Self {
        let exact = std::mem::take(&mut self.exact)
            .into_iter()
            .map(|point| resolve(point.name()).unwrap_or(point))
            .collect::<Vec<_>>();
        let generators = std::mem::take(&mut self.generators)
            .into_iter()
            .map(|generator| resolve(generator.name()).unwrap_or(generator))
            .collect::<Vec<_>>();
        Self::new(exact, generators)
    }

    pub fn label(&self) -> Option<String> {
        let mut names = self
            .exact
            .iter()
            .chain(&self.generators)
            .map(|ty| ty.name().name())
            .collect::<Vec<_>>();
        names.sort_unstable();
        (!names.is_empty()).then(|| names.join(" | "))
    }

    pub fn subset_label(
        &self,
        has_strict_member_above: impl Fn(&ObjectName) -> bool,
    ) -> Option<String> {
        let mut names = self
            .generators
            .iter()
            .map(|generator| {
                if has_strict_member_above(generator.name()) {
                    format!("↑{}", generator.name().name())
                } else {
                    generator.name().name().to_string()
                }
            })
            .chain(
                self.exact
                    .iter()
                    .map(|point| point.name().name().to_string()),
            )
            .collect::<Vec<_>>();
        names.sort_unstable();
        (!names.is_empty()).then(|| names.join(" | "))
    }

    pub fn possibility_by(
        &self,
        candidate: &ObjectName,
        evidence: impl Fn(&ObjectName, &ObjectName) -> SubtypeEvidence,
    ) -> SubtypeEvidence {
        let mut result = SubtypeEvidence::Disproven;
        if self.exact.iter().any(|point| point.name() == candidate) {
            return SubtypeEvidence::Proven;
        }
        for generator in &self.generators {
            match evidence(candidate, generator.name()) {
                SubtypeEvidence::Proven => return SubtypeEvidence::Proven,
                SubtypeEvidence::Unknown => result = SubtypeEvidence::Unknown,
                SubtypeEvidence::Disproven => {}
            }
        }
        result
    }

    fn new(
        exact: impl IntoIterator<Item = Type>,
        generators: impl IntoIterator<Item = Type>,
    ) -> Self {
        let mut range = Self::default();
        for point in exact {
            range.insert_exact(point);
        }
        for generator in generators {
            range.insert_generator(generator);
        }
        range
    }

    fn contains(&self, candidate: &Type) -> bool {
        self.exact.contains(candidate)
            || self
                .generators
                .iter()
                .any(|generator| generator <= candidate)
    }

    fn insert_exact(&mut self, point: Type) {
        if !self.contains(&point) {
            self.exact.insert(point);
        }
    }

    fn insert_generator(&mut self, generator: Type) {
        if self
            .generators
            .iter()
            .any(|existing| existing <= &generator)
        {
            return;
        }
        self.exact.retain(|point| {
            !matches!(
                generator.partial_cmp(point),
                Some(Ordering::Less | Ordering::Equal)
            )
        });
        self.generators.retain(|existing| {
            !matches!(
                generator.partial_cmp(existing),
                Some(Ordering::Less | Ordering::Equal)
            )
        });
        self.generators.insert(generator);
    }
}

impl BitOr for TypeRange {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self::new(
            self.exact.into_iter().chain(rhs.exact),
            self.generators.into_iter().chain(rhs.generators),
        )
    }
}

impl BitAnd for TypeRange {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self {
        let mut intersection = Self::default();
        for point in &self.exact {
            if rhs.contains(point) {
                intersection.insert_exact(point.clone());
            }
        }
        for point in &rhs.exact {
            if self.contains(point) {
                intersection.insert_exact(point.clone());
            }
        }
        for left in &self.generators {
            for right in &rhs.generators {
                match left.partial_cmp(right) {
                    Some(Ordering::Less) => intersection.insert_generator(right.clone()),
                    Some(Ordering::Equal | Ordering::Greater) => {
                        intersection.insert_generator(left.clone());
                    }
                    None => {}
                }
            }
        }
        intersection
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ty(path: &[&str]) -> Type {
        Type {
            name: ObjectName::new(*path.last().expect("type path")),
            ancestry: path
                .iter()
                .map(|name| TypeKey(Arc::from(*name)))
                .collect::<Vec<_>>()
                .into(),
        }
    }

    fn types(items: impl IntoIterator<Item = Type>) -> HashSet<Type> {
        items.into_iter().collect()
    }

    #[test]
    fn ancestry_is_ordered_from_thing_to_subtype() {
        let thing = ty(&["Thing"]);
        let list = ty(&["Thing", "VisibleList", "List"]);
        let mutable_list = ty(&["Thing", "VisibleList", "List", "MutableList"]);
        let array = ty(&["Thing", "VisibleList", "Array"]);

        assert!(thing < list);
        assert!(list < mutable_list);
        assert_eq!(list.partial_cmp(&array), None);
    }

    #[test]
    fn generators_are_normalized_to_minimal_incomparable_types() {
        let list = ty(&["Thing", "VisibleList", "List"]);
        let mutable_list = ty(&["Thing", "VisibleList", "List", "MutableList"]);
        let array = ty(&["Thing", "VisibleList", "Array"]);
        let range = TypeRange::new([], [mutable_list, list.clone(), array.clone()]);

        assert_eq!(range.generators, types([list, array]));
    }

    #[test]
    fn inserting_a_generator_removes_only_covered_exact_types() {
        let list = ty(&["Thing", "VisibleList", "List"]);
        let mutable_list = ty(&["Thing", "VisibleList", "List", "MutableList"]);
        let array = ty(&["Thing", "VisibleList", "Array"]);
        let sequence = ty(&["Thing", "VisibleList", "Sequence"]);
        let range = TypeRange::new(
            [mutable_list, array.clone(), sequence.clone()],
            [list.clone()],
        );

        assert_eq!(range.exact, types([array, sequence]));
        assert_eq!(range.generators, types([list]));
    }

    #[test]
    fn comparable_exact_types_remain_distinct_alternatives() {
        let list = ty(&["Thing", "VisibleList", "List"]);
        let mutable_list = ty(&["Thing", "VisibleList", "List", "MutableList"]);
        let range = TypeRange::new([list.clone(), mutable_list.clone()], []);

        assert_eq!(range.exact, types([list, mutable_list]));
    }

    #[test]
    fn union_preserves_incomparable_generators_without_widening() {
        let list = ty(&["Thing", "VisibleList", "List"]);
        let array = ty(&["Thing", "VisibleList", "Array"]);
        let sequence = ty(&["Thing", "VisibleList", "Sequence"]);
        let union = TypeRange::upward_type(list.clone()) | TypeRange::upward_type(array.clone());

        assert_eq!(union.generators, types([list, array]));
        assert!(!union.contains(&sequence));
    }

    #[test]
    fn intersection_of_incomparable_generators_is_empty() {
        let lists = TypeRange::upward_type(ty(&["Thing", "VisibleList", "List"]));
        let arrays = TypeRange::upward_type(ty(&["Thing", "VisibleList", "Array"]));

        assert_eq!(lists & arrays, TypeRange::default());
    }

    #[test]
    fn intersection_keeps_the_more_specific_comparable_generator() {
        let things = TypeRange::upward_type(ty(&["Thing"]));
        let lists = TypeRange::upward_type(ty(&["Thing", "VisibleList", "List"]));

        let intersection = things & lists.clone();

        assert_eq!(intersection, lists);
    }

    #[test]
    fn intersection_filters_exact_alternatives_by_a_generator() {
        let list = ty(&["Thing", "VisibleList", "List"]);
        let array = ty(&["Thing", "VisibleList", "Array"]);
        let alternatives = TypeRange::new([list.clone(), array], []);
        let lists = TypeRange::upward_type(list.clone());

        let intersection = alternatives & lists;

        assert_eq!(intersection.exact, types([list]));
        assert!(intersection.generators.is_empty());
    }
}
