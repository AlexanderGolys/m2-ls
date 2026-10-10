//! Static type inference over the canonical object facts.
//!
//! This module does not evaluate Macaulay2. It queries the active object
//! environment only to answer conservative type and dispatch questions.

use crate::builtin_index::{CallableInfo, MethodSignature};
use tower_lsp::lsp_types::Position;

use crate::object_registry::{
    ObjectKnowledge, ObjectName, ObjectRegistry, ObjectRegistryView, TypeId,
};

mod type_range;

pub use type_range::{Type, TypeRange};

pub type InferredType = TypeRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeRole {
    Boolean,
    Function,
    MethodFunction,
    Package,
    Ring,
    String,
    Symbol,
    Thing,
    Type,
    VisibleList,
}

/// Evidence available for one nominal subtype question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtypeEvidence {
    Proven,
    Disproven,
    Unknown,
}

impl TypeRole {
    pub fn object_name(self) -> ObjectName {
        ObjectName::new(match self {
            Self::Boolean => "Boolean",
            Self::Function => "Function",
            Self::MethodFunction => "MethodFunction",
            Self::Package => "Package",
            Self::Ring => "Ring",
            Self::String => "String",
            Self::Symbol => "Symbol",
            Self::Thing => "Thing",
            Self::Type => "Type",
            Self::VisibleList => "VisibleList",
        })
    }
}

/// One statically known option key and literal value at a call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiteralOption {
    pub option: ObjectName,
    pub value: ObjectName,
}

/// The semantic facts analysis needs from an external object registry.
///
/// Keeping this as a narrow trait decouples the analysis engine from registry
/// storage and resolution order. The complete [`ObjectRegistry`], an ordered
/// package view, and syntax-only test knowledge all implement the same contract.
pub trait TypeKnowledge: ObjectKnowledge + PositionedTypeKnowledge {
    /// Whether external type facts are available for diagnostics.
    fn is_available(&self) -> bool {
        true
    }

    /// Whether a later package inclusion shadows a global source definition of
    /// `name` made at `source_position`.
    fn shadows_source(&self, _name: &ObjectName, _source_position: Position) -> bool {
        false
    }

    fn subtype_evidence(&self, child: &ObjectName, parent: &ObjectName) -> SubtypeEvidence {
        let Some((child, parent)) = self
            .resolve_type_id(child)
            .zip(self.resolve_type_id(parent))
        else {
            return SubtypeEvidence::Unknown;
        };
        if self.is_subtype_id(&child, &parent) {
            SubtypeEvidence::Proven
        } else {
            SubtypeEvidence::Disproven
        }
    }

    fn is_subtype(&self, child: &ObjectName, parent: &ObjectName) -> bool {
        self.subtype_evidence(child, parent) == SubtypeEvidence::Proven
    }

    fn has_type_role(&self, candidate: &ObjectName, role: TypeRole) -> bool {
        self.type_role_evidence(candidate, role) == SubtypeEvidence::Proven
    }

    fn type_role_evidence(&self, candidate: &ObjectName, role: TypeRole) -> SubtypeEvidence {
        self.subtype_evidence(candidate, &role.object_name())
    }

    fn type_role_id(&self, role: TypeRole) -> Option<TypeId> {
        self.resolve_type_id(&role.object_name())
    }
}

/// Supplies the object environment effective at one source position.
pub trait PositionedTypeKnowledge {
    type Knowledge<'a>: TypeKnowledge + PositionedTypeKnowledge
    where
        Self: 'a;

    fn at_position(&self, position: Position) -> Self::Knowledge<'_>;
}

impl<T: PositionedTypeKnowledge + ?Sized> PositionedTypeKnowledge for &T {
    type Knowledge<'a>
        = T::Knowledge<'a>
    where
        Self: 'a;

    fn at_position(&self, position: Position) -> Self::Knowledge<'_> {
        T::at_position(self, position)
    }
}

impl PositionedTypeKnowledge for ObjectRegistry {
    type Knowledge<'a> = ObjectRegistryView<'a>;

    fn at_position(&self, position: Position) -> Self::Knowledge<'_> {
        self.at(position)
    }
}

impl PositionedTypeKnowledge for ObjectRegistryView<'_> {
    type Knowledge<'a>
        = Self
    where
        Self: 'a;

    fn at_position(&self, position: Position) -> Self::Knowledge<'_> {
        self.at(position)
    }
}

impl TypeKnowledge for ObjectRegistry {}

impl TypeKnowledge for ObjectRegistryView<'_> {
    fn shadows_source(&self, name: &ObjectName, source_position: Position) -> bool {
        ObjectRegistryView::shadows_source(self, name, source_position)
    }
}

impl<T: TypeKnowledge + ?Sized> TypeKnowledge for &T {
    fn is_available(&self) -> bool {
        T::is_available(self)
    }

    fn shadows_source(&self, name: &ObjectName, source_position: Position) -> bool {
        T::shadows_source(self, name, source_position)
    }
}

impl CallableInfo {
    /// Resolve a method's effective codomain and whether it is specialized.
    pub fn effective_codomain<'a>(
        &'a self,
        method: &'a MethodSignature,
    ) -> Option<(&'a TypeId, bool)> {
        method
            .codomain
            .as_ref()
            .map(|codomain| (codomain, true))
            .or_else(|| {
                self.typical_value
                    .as_ref()
                    .map(|codomain| (codomain, false))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_subtype_holds_for_the_immediate_parent_edge() {
        // Regression: the corpus `ancestors` field omits the immediate parent, so
        // a lattice built from it alone made `is_subtype(child, parent)` succeed
        // only reflexively (`new Type` worked, `new SelfInitializingType` did not).
        let builtins = ObjectRegistry::load(include_str!("./data/m2-index.jsonl"));

        // Direct parent edges (verified against M2): SelfInitializingType <: Type,
        // Array <: VisibleList.
        let subtype = |child: &str, parent: &str| {
            builtins.is_subtype(&ObjectName::new(child), &ObjectName::new(parent))
        };
        assert!(subtype("SelfInitializingType", "Type"));
        assert!(subtype("Array", "VisibleList"));
        // Transitive edges still hold, and unrelated types stay unrelated.
        assert!(subtype("Array", "Thing"));
        assert!(!subtype("Array", "Type"));
    }
}
